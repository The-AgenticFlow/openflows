---
name: harness-protocol
description: Shared protocol for agent pairwise harness execution and coordination.
---

# Shared Harness Protocol

This skill defines the protocol for orchestrating pairwise agent execution (harness), enabling coordinated multi-agent workflows where agents work together in harness pairs.

## Protocol Overview

The harness protocol enables structured coordination between an orchestrating agent and a worker agent in a paired execution context. The orchestrator manages workflow state while the worker executes tasks within assigned constraints.

## Execution Flow

1. **Harness Setup**: Establish state contracts and constraints
2. **Task Assignment**: Define boundaries and expectations for worker execution
3. **Progress Monitoring**: Track state transitions and handle deviations
4. **Result Aggregation**: Collect and validate worker outputs

## Coordination Rules

- **State Contracts**: All shared state must conform to declared type schemas
- **Scope Enforcement**: Workers operate within defined boundaries
- **Recovery Handling**: Automatic retry with exponential backoff for transient failures
- **State Validation**: Confirm state changes before proceeding to next phase

## Error Handling

- Transient failures: Retry withbackoff
- Validation failures: Log and escalate to orchestrator
- Timeout: Trigger re-evaluation of current state