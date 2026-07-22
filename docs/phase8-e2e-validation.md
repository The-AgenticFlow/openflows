# Phase 8 — End-to-End Validation

> **Prerequisite:** A live Coder stack must be running with:
> - `docker compose up -d` (Coder + Postgres + Redis)
> - At least one LLM model configured in the Coder dashboard
> - GitHub external auth configured (OAuth app credentials in `.env`)
> - A target GitHub repository with issues enabled

## 8.1 Bootstrap Idempotency

```bash
openflows bootstrap
# Expected: admin created, templates pushed, LLM config verified, external auth OK

openflows bootstrap
# Expected: second run is a no-op with "already configured" output for each step
```

## 8.2 Happy Path (Full Flow)

```bash
# 1. Add tenant
openflows tenant add owner/repo --name acme
# Expected: prints tenant created, workspace ID, OAuth link prompt

# 2. Complete GitHub OAuth link in browser (per printed URL), then press Enter

# 3. Create a labeled GitHub issue in the target repo
#    Labels should be recognized by the Controller (or any open issue)

# 4. Watch the Coder dashboard for workspaces to appear:
#    - forge-T-XXX workspace
#    - Chat bound to that workspace (Coder Agents UI)
#    - Agent calls openflows-harness dispatch read (visible in chat transcript)

# 5. Wait for PR to be opened by forge
#    Check: openflows status --tenant acme
#    Should show: ticket in progress, forge chat active, pending_prs growing

# 6. Sentinel review: sentinel-T-XXX workspace + chat appears
#    Agent reads handoff, submits review via harness review submit
#    On approve: routes to vessel

# 7. Vessel merge: vessel-T-XXX workspace + chat appears (or vessel runs Controller-side)
#    CI polled, PR merged, deployment key written
#    Check: openflows status --tenant acme --json
#    Should show: ticket merged, worker_slots freed, no pending PRs

# 8. Post-merge cleanup verification:
#    - forge/sentinel/vessel chats are archived (no longer in active chats list)
#    - forge/sentinel/vessel workspaces are deleted (not in Coder workspace list)
#    - worker slot shows Idle
#    - openflows status shows merged state
```

## 8.3 Self-Healing

```bash
# 1. Start a new ticket and wait for forge workspace to appear
# 2. Kill the forge workspace mid-task (Coder dashboard → stop/delete the workspace)
# 3. Watch the Controller log (nexus workspace): within ~90s it should detect stale heartbeat
#    Expected: "heartbeat stale after Xs for ws Y" → recreate workspace + fresh chat
# 4. Verify forge workspace reappears and chat resumes

# 5. Repeat kill 3× on the same ticket
#    Expected: after 3rd failure, ticket status → awaiting_human
#    Check: openflows status --tenant acme
#    Should show: ticket in AwaitingHuman status, attempts=3
#    Notifier should fire (if webhook configured)
```

## 8.4 Tenant Isolation

```bash
# 1. Add second tenant
openflows tenant add other-org/other-repo --name beta
# 2. Complete OAuth for beta tenant

# 3. Verify Redis keys are namespaced:
redis-cli KEYS "ns:acme:*" | wc -l   # should be > 0
redis-cli KEYS "ns:beta:*" | wc -l   # should be > 0
redis-cli KEYS "ns:acme:*" | grep "ns:beta" | wc -l  # should be 0 (no cross-contamination)

# 4. Verify Coder workspace ownership:
#    - beta tenant's owner should NOT see acme workspaces in their Coder dashboard
#    - Each tenant has its own nexus workspace (openflows-nexus-acme, openflows-nexus-beta)
```

## 8.5 Security Audit

```bash
# 1. Inspect a worker workspace environment (forge/sentinel/vessel)
#    SSH into the workspace via Coder dashboard or coder ssh, then:
env | grep -E "ANTHROPIC|OPENAI|GITHUB_TOKEN|GITHUB_PERSONAL_ACCESS"
# Expected: no matches — worker workspaces have NO LLM keys, NO GitHub tokens

# 2. Verify tenant-owner token is scoped:
#    Get the tenant's Coder session token from the nexus workspace env
#    Try to call an admin endpoint:
curl -H "Coder-Session-Token: $TENANT_TOKEN" \
  $CODER_URL/api/v2/users/first
# Expected: 403 Forbidden (tenant owner is not admin)

# 3. Malformed harness input test:
#    SSH into a forge workspace, then:
REDIS_URL=redis://redis:6379 OPENFLOWS_TENANT=acme OPENFLOWS_TICKET=T-001 OPENFLOWS_ROLE=forge \
  openflows-harness status set invalid_phase
# Expected: exit code non-zero, stderr: "Invalid phase 'invalid_phase'. Valid phases: ..."
# Verify: redis-cli GET "ns:acme:ticket:T-001:status" should be null (nothing written)
```

## Acceptance Criteria

| Test | Pass Criteria |
|------|--------------|
| 8.1 | Second bootstrap prints "already configured" or equivalent no-op messages |
| 8.2 | Full cycle: issue → PR → review → merge → archived/deleted → status shows merged |
| 8.3 | Workspace crash auto-recovers once; 3rd crash → awaiting_human + notifier |
| 8.4 | Redis keys never cross tenant boundaries; Coder workspace ownership is tenant-scoped |
| 8.5 | No LLM/GitHub tokens in worker env; tenant token cannot admin; bad harness input → non-zero exit, no write |
