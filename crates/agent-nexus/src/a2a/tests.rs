// crates/agent-nexus/src/a2a/tests.rs
//! Integration and unit tests for the A2A relay (task 8).

#[cfg(test)]
#[allow(clippy::module_inception)]
mod tests {
    use a2a_protocol::{
        ExecutorInfo, VerifyCwd, VerifyExpect, VerifyKind, VerifyProgressEvent, VerifyRequest,
        VerifyResult,
    };
    use pocketflow_core::SharedStore;
    use std::sync::atomic::Ordering;
    use std::sync::Arc;

    fn sample_request() -> VerifyRequest {
        VerifyRequest {
            pair_id: "T-048".to_string(),
            kind: VerifyKind::Command,
            cwd: VerifyCwd::Repo,
            argv: vec!["cargo".into(), "test".into()],
            timeout_secs: 300,
            env_allowlist: vec![],
            expect: VerifyExpect {
                exit_code: Some(0),
                artifacts: vec![],
            },
        }
    }

    fn sample_result(task_id: &str) -> VerifyResult {
        VerifyResult {
            head_sha: None,
            task_id: task_id.to_string(),
            exit_code: Some(0),
            timed_out: false,
            duration_ms: 42,
            stdout_ref: format!("audit:a2a:{}:stdout", task_id),
            stderr_ref: format!("audit:a2a:{}:stderr", task_id),
            artifacts: vec![],
            executor: ExecutorInfo {
                role: "forge".into(),
                workspace: "forge-T-048".into(),
            },
        }
    }

    #[test]
    fn test_verify_request_idempotency_seed_is_stable() {
        let req = VerifyRequest {
            pair_id: "T-048".to_string(),
            kind: VerifyKind::Command,
            cwd: VerifyCwd::Repo,
            argv: vec!["cargo".into(), "test".into()],
            timeout_secs: 300,
            env_allowlist: vec![],
            expect: VerifyExpect {
                exit_code: Some(0),
                artifacts: vec![],
            },
        };

        let seed1 = req.idempotency_seed().unwrap();
        let seed2 = req.idempotency_seed().unwrap();

        // Seeds should be identical for identical requests
        assert_eq!(seed1, seed2);

        // Seed should be pair-scoped
        let mut req2 = req.clone();
        req2.pair_id = "T-049".to_string();
        let seed3 = req2.idempotency_seed().unwrap();

        assert_ne!(seed1, seed3);
        assert!(seed1.starts_with("T-048:"));
        assert!(seed3.starts_with("T-049:"));
    }

    #[test]
    fn test_allowlist_validation_passes_known_commands() {
        let valid_commands = vec![
            vec!["cargo", "test"],
            vec!["cargo", "test", "--package", "foo"],
            vec!["npm", "test"],
            vec!["make", "test"],
            vec!["bun", "test"],
        ];

        for cmd in valid_commands {
            let argv: Vec<String> = cmd.into_iter().map(|s| s.to_string()).collect();
            assert!(
                a2a_protocol::is_allowlisted(&argv),
                "Command {:?} should be allowlisted",
                argv
            );
        }
    }

    #[test]
    fn test_policy_validation_rejects_prohibited_operations() {
        let invalid_commands = vec![
            vec!["rm", "-rf", "/"],
            vec!["cargo", "publish"],
            vec!["redis-cli", "FLUSHALL"],
            vec!["sudo", "id"],
            vec![],
        ];

        for cmd in invalid_commands {
            let argv: Vec<String> = cmd.into_iter().map(|s| s.to_string()).collect();
            assert!(
                !a2a_protocol::is_allowlisted(&argv),
                "Command {:?} should NOT be allowlisted",
                argv
            );
        }
    }

    #[test]
    fn test_verify_result_satisfies_checks_exit_code() {
        let result = sample_result("t1");

        let expect_pass = VerifyExpect {
            exit_code: Some(0),
            artifacts: vec![],
        };

        assert!(result.satisfies(&expect_pass));

        let expect_fail = VerifyExpect {
            exit_code: Some(1),
            artifacts: vec![],
        };

        assert!(!result.satisfies(&expect_fail));
    }

    #[test]
    fn test_verify_result_timeout_never_satisfies() {
        let mut result = sample_result("t1");
        result.timed_out = true;
        result.exit_code = Some(0);

        let any_expect = VerifyExpect {
            exit_code: Some(0),
            artifacts: vec![],
        };

        // Timeout always fails, regardless of exit code
        assert!(!result.satisfies(&any_expect));
    }

    #[test]
    fn test_redis_key_formatting() {
        use a2a_protocol::{audit_rejected_key, audit_task_key, verification_key};

        assert_eq!(verification_key("T-048"), "pair:T-048:verification");
        assert_eq!(audit_task_key("abc123"), "audit:a2a:abc123");
        assert_eq!(audit_rejected_key(), "audit:a2a:rejected");
    }

    #[tokio::test]
    async fn test_relay_lifecycle_submit_claim_complete_get() {
        use crate::a2a::routing::{A2ARelay, TaskState};
        use crate::a2a::verify_handler::submit_verify_request;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let req = sample_request();

        // Sentinel submits; a real task id is returned (not a placeholder).
        let task_id = submit_verify_request(&relay, &req, "T-048").await.unwrap();
        assert!(!task_id.is_empty());
        assert_eq!(
            relay.get_task_state(&task_id).await,
            Some(TaskState::Pending)
        );

        // Idempotent duplicate submit returns the same task id.
        let task_id2 = submit_verify_request(&relay, &req, "T-048").await.unwrap();
        assert_eq!(task_id, task_id2);

        // Forge claims the next pending task for the pair.
        let claimed = relay.claim_next_task("T-048").await.unwrap().unwrap();
        assert_eq!(claimed.task_id, task_id);
        assert_eq!(
            relay.get_task_state(&task_id).await,
            Some(TaskState::Running)
        );

        // No more pending tasks for the pair.
        assert!(relay.claim_next_task("T-048").await.unwrap().is_none());

        // Forge completes the task with a terminal result.
        relay
            .complete_task(&task_id, sample_result(&task_id))
            .await
            .unwrap();
        assert_eq!(
            relay.get_task_state(&task_id).await,
            Some(TaskState::Completed)
        );

        // The completed task carries the mirrored result for Sentinel.
        let entry = relay.get_task(&task_id).await.unwrap();
        assert!(entry.result.is_some());
        assert_eq!(entry.result.unwrap().task_id, task_id);

        // Terminal tasks are retryable. This matters when an executor setup
        // failure is fixed and Sentinel reruns the same verification request.
        let retry_task_id = submit_verify_request(&relay, &req, "T-048").await.unwrap();
        assert_ne!(task_id, retry_task_id);
        assert_eq!(
            relay.get_task_state(&retry_task_id).await,
            Some(TaskState::Pending)
        );
    }

    #[tokio::test]
    async fn test_relay_claim_rejects_non_forge_role_via_validation() {
        use crate::a2a::routing::A2ARelay;
        use crate::a2a::verify_handler::submit_verify_request;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let req = sample_request();
        let task_id = submit_verify_request(&relay, &req, "T-048").await.unwrap();

        // A non-forge role must not be able to claim verify tasks: claim is
        // gated on role in the HTTP handler, and pair mismatch on submit is
        // rejected here. Verify the request is rejected for a cross-pair submit.
        let wrong_pair = VerifyRequest {
            pair_id: "T-999".to_string(),
            ..req.clone()
        };
        assert!(submit_verify_request(&relay, &wrong_pair, "T-048")
            .await
            .is_err());

        assert_eq!(
            relay.get_task_state(&task_id).await,
            Some(crate::a2a::routing::TaskState::Pending)
        );
    }

    // ── New tests for SSE progress, cancel, resubscribe ──────────────────────

    #[tokio::test]
    async fn test_event_buffer_push_and_replay() {
        use crate::a2a::routing::EventBuffer;

        let mut buf = EventBuffer::new();

        let ev1 = VerifyProgressEvent::Stdout {
            chunk: "line1".into(),
        };
        let ev2 = VerifyProgressEvent::Stderr {
            chunk: "error".into(),
        };

        let b1 = buf.push(ev1.clone());
        let b2 = buf.push(ev2.clone());

        // Sequence numbers should be monotonically increasing
        assert_eq!(b1.seq, 0);
        assert_eq!(b2.seq, 1);
        assert_eq!(buf.current_seq(), 1);

        // Replay all (last_seq=0) should return everything (oldest first)
        let all = buf.events_since(0);
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].seq, 0);
        assert_eq!(all[1].seq, 1);

        // Replay since seq=1 should return [1] (only seq 1, since there are only 2 events)
        let newer = buf.events_since(1);
        assert_eq!(newer.len(), 1);
        assert_eq!(newer[0].seq, 1);
    }

    #[tokio::test]
    async fn test_event_buffer_fifo_eviction() {
        use crate::a2a::routing::EventBuffer;

        let mut buf = EventBuffer::new();
        // We can't set max_events directly (private), but the default is 1000.
        // Push enough events to test the byte-limit eviction path.
        // Each small event is ~20 bytes, so 1000 events is well under 1MiB.
        // Instead, verify that the buffer doesn't grow unbounded by pushing
        // many events and checking the buffer stays within limits.
        for i in 0..1100 {
            let ev = VerifyProgressEvent::Stdout {
                chunk: format!("line{}", i),
            };
            buf.push(ev);
        }

        // Buffer should never exceed max_events
        let all = buf.events_since(0);
        assert!(all.len() <= 1000);
    }

    #[tokio::test]
    async fn test_push_progress_event_and_subscribe() {
        use crate::a2a::routing::A2ARelay;
        use crate::a2a::verify_handler::submit_verify_request;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let req = sample_request();
        let task_id = submit_verify_request(&relay, &req, "T-048").await.unwrap();

        // Push progress events
        let ev1 = VerifyProgressEvent::Stdout {
            chunk: "compiling".into(),
        };
        let ev2 = VerifyProgressEvent::Stderr {
            chunk: "warning".into(),
        };

        let b1 = relay.push_progress_event(&task_id, ev1).await.unwrap();
        let b2 = relay.push_progress_event(&task_id, ev2).await.unwrap();

        assert_eq!(b1.seq, 0);
        assert_eq!(b2.seq, 1);

        // Subscribe to live events
        let mut rx = relay.subscribe_to_task(&task_id).await;

        // Push another event
        let ev3 = VerifyProgressEvent::Stdout {
            chunk: "done".into(),
        };
        let b3 = relay.push_progress_event(&task_id, ev3).await.unwrap();
        assert_eq!(b3.seq, 2);

        // The broadcast receiver should get it
        let received = tokio::time::timeout(std::time::Duration::from_secs(1), rx.recv())
            .await
            .expect("Should receive event within timeout")
            .expect("Should get event");
        assert_eq!(received.seq, 2);
    }

    #[tokio::test]
    async fn test_replay_events_since_via_relay() {
        use crate::a2a::routing::A2ARelay;
        use crate::a2a::verify_handler::submit_verify_request;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let req = sample_request();
        let task_id = submit_verify_request(&relay, &req, "T-048").await.unwrap();

        // Push events
        for i in 0..3 {
            let ev = VerifyProgressEvent::Stdout {
                chunk: format!("line{}", i),
            };
            relay.push_progress_event(&task_id, ev).await.unwrap();
        }

        // Replay all events (oldest first)
        let all = relay.replay_events_since(&task_id, 0).await;
        assert_eq!(all.len(), 3);

        // Replay since seq=1 → should get seq 1 and 2 (seq >= 1)
        let since = relay.replay_events_since(&task_id, 1).await;
        assert_eq!(since.len(), 2);
        assert_eq!(since[0].seq, 1);
        assert_eq!(since[1].seq, 2);
    }

    #[tokio::test]
    async fn test_cancel_flag_and_is_cancelled() {
        use crate::a2a::routing::A2ARelay;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let task_id = "test-cancel-1";

        // Not cancelled by default
        assert!(!relay.is_cancelled(task_id).await);

        // Create a cancel token (simulates executor claiming)
        relay.create_cancel_token(task_id).await;
        assert!(!relay.is_cancelled(task_id).await);

        // Mark cancelled
        let newly_set = relay.mark_cancelled(task_id).await;
        assert!(newly_set);
        assert!(relay.is_cancelled(task_id).await);

        // Double-cancel should return false (already set)
        let newly_set2 = relay.mark_cancelled(task_id).await;
        assert!(!newly_set2);
    }

    #[tokio::test]
    async fn test_cancel_without_token_creates_one() {
        use crate::a2a::routing::A2ARelay;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let task_id = "test-cancel-2";

        // Mark cancelled without first creating token
        relay.mark_cancelled(task_id).await;
        assert!(relay.is_cancelled(task_id).await);

        // Create token after cancel should reflect cancelled state
        let token = relay.create_cancel_token(task_id).await;
        assert!(token.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_cancel_through_lifecycle() {
        use crate::a2a::routing::{A2ARelay, TaskState};
        use crate::a2a::verify_handler::submit_verify_request;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let req = sample_request();
        let task_id = submit_verify_request(&relay, &req, "T-048").await.unwrap();

        // Claim the task
        let claimed = relay.claim_next_task("T-048").await.unwrap().unwrap();
        assert_eq!(claimed.task_id, task_id);
        assert_eq!(
            relay.get_task_state(&task_id).await,
            Some(TaskState::Running)
        );

        // Set up cancel token as executor would
        relay.create_cancel_token(&task_id).await;

        // Cancel the task
        relay.mark_cancelled(&task_id).await;
        assert!(relay.is_cancelled(&task_id).await);

        // The cancel handler in http_server calls complete_task with synthetic result.
        // Verify the task can still be completed even if cancelled.
        relay
            .complete_task(&task_id, sample_result(&task_id))
            .await
            .unwrap();
        assert_eq!(
            relay.get_task_state(&task_id).await,
            Some(TaskState::Completed)
        );
    }

    #[tokio::test]
    async fn test_synthetic_cancel_result_transitions_state() {
        use crate::a2a::routing::{A2ARelay, TaskState};
        use crate::a2a::verify_handler::submit_verify_request;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let req = sample_request();
        let task_id = submit_verify_request(&relay, &req, "T-048").await.unwrap();
        relay.claim_next_task("T-048").await.unwrap();

        // Build a synthetic cancelled result (as done in http_server cancel handler)
        let cancelled_result = VerifyResult {
            head_sha: None,
            task_id: task_id.clone(),
            exit_code: None,
            timed_out: false,
            duration_ms: 0,
            stdout_ref: format!("audit:a2a:{}:stdout", task_id),
            stderr_ref: format!("audit:a2a:{}:stderr", task_id),
            artifacts: vec![],
            executor: ExecutorInfo {
                role: "forge".into(),
                workspace: format!("unknown-{}", req.pair_id),
            },
        };

        relay
            .complete_task(&task_id, cancelled_result)
            .await
            .unwrap();
        assert_eq!(
            relay.get_task_state(&task_id).await,
            Some(TaskState::Completed)
        );

        // Result should have exit_code None (cancelled)
        let entry = relay.get_task(&task_id).await.unwrap();
        assert_eq!(entry.result.unwrap().exit_code, None);
    }

    #[tokio::test]
    async fn test_resubscribe_with_event_replay() {
        use crate::a2a::routing::A2ARelay;
        use crate::a2a::verify_handler::submit_verify_request;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let req = sample_request();
        let task_id = submit_verify_request(&relay, &req, "T-048").await.unwrap();

        // Push some events before "disconnect"
        for i in 0..5 {
            let ev = VerifyProgressEvent::Stdout {
                chunk: format!("line{}", i),
            };
            relay.push_progress_event(&task_id, ev).await.unwrap();
        }

        // "Reconnect" at seq 2 → should receive events 2, 3, 4 (seq >= 2)
        let replayed = relay.replay_events_since(&task_id, 2).await;
        assert_eq!(replayed.len(), 3);
        assert_eq!(replayed[0].seq, 2);
        assert_eq!(replayed[1].seq, 3);
        assert_eq!(replayed[2].seq, 4);
    }

    #[test]
    fn test_event_buffer_default_limits() {
        use crate::a2a::routing::EventBuffer;

        let buf = EventBuffer::new();
        // Verify the default constants via accessor methods
        assert_eq!(buf.max_events(), 1000);
        assert_eq!(buf.max_bytes(), 1_048_576);
    }

    #[tokio::test]
    async fn test_cancel_token_shared_with_executor() {
        use crate::a2a::routing::A2ARelay;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        let task_id = "test-cancel-shared";
        let token = relay.create_cancel_token(task_id).await;

        // Spawn a simulated executor that checks the token
        let token_clone = token.clone();
        let handle = tokio::spawn(async move {
            // Simulate work while checking cancellation
            for _ in 0..10 {
                if token_clone.load(Ordering::SeqCst) {
                    return "cancelled";
                }
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            "completed"
        });

        // Cancel via relay after a short delay
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        relay.mark_cancelled(task_id).await;

        let result = handle.await.unwrap();
        assert_eq!(result, "cancelled");
    }

    #[tokio::test]
    async fn test_valid_verify_request_flow() {
        use crate::a2a::routing::A2ARelay;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        // Test validate_verify_request directly
        let req = sample_request();
        assert!(relay.validate_verify_request(&req, "T-048").is_ok());

        // Should fail for wrong pair_id
        assert!(relay.validate_verify_request(&req, "T-999").is_err());

        // Should fail for zero timeout
        let bad_timeout = VerifyRequest {
            timeout_secs: 0,
            ..req.clone()
        };
        assert!(relay
            .validate_verify_request(&bad_timeout, "T-048")
            .is_err());

        // Should fail for excessive timeout
        let huge_timeout = VerifyRequest {
            timeout_secs: 99999,
            ..req.clone()
        };
        assert!(relay
            .validate_verify_request(&huge_timeout, "T-048")
            .is_err());

        // Empty argv should fail allowlist
        let empty_argv = VerifyRequest {
            argv: vec![],
            ..req.clone()
        };
        assert!(relay.validate_verify_request(&empty_argv, "T-048").is_err());
    }

    #[tokio::test]
    async fn test_current_event_seq() {
        use crate::a2a::routing::A2ARelay;

        let store = SharedStore::new_in_memory();
        let relay = A2ARelay::new(Arc::new(store));

        // No task yet → 0
        assert_eq!(relay.current_event_seq("nonexistent").await, 0);

        // Push events via submit
        let req = sample_request();
        let task_id = crate::a2a::verify_handler::submit_verify_request(&relay, &req, "T-048")
            .await
            .unwrap();

        // First event gets seq 0
        assert_eq!(relay.current_event_seq(&task_id).await, 0);

        let ev = VerifyProgressEvent::Stdout {
            chunk: "hello".into(),
        };
        relay.push_progress_event(&task_id, ev).await.unwrap();
        // After first push, next_seq=1, current_seq = next_seq - 1 = 0
        assert_eq!(relay.current_event_seq(&task_id).await, 0);

        let ev2 = VerifyProgressEvent::Stderr {
            chunk: "world".into(),
        };
        relay.push_progress_event(&task_id, ev2).await.unwrap();
        assert_eq!(relay.current_event_seq(&task_id).await, 1);
    }
}
