---
name: forge-planning
description: Planning skill for the FORGE builder agent
---

# FORGE Planning Skill

## Writing PLAN.md

Before any implementation, write a plan.
Use the `/plan` command to structure it correctly.

## What a good plan contains

1. **Your understanding of the ticket** - in your own words
2. **Technical approach** - follows `orchestration/agent/arch/patterns.md` (if it exists)
3. **Explicit segment breakdown** - each segment is independently testable
4. **Definition of done per segment** - specific and verifiable
5. **List of files you will create or modify**
6. **Risk areas** - things you are uncertain about
7. **Questions for SENTINEL** - clarifications needed before starting

## Segment sizing

A good segment:
- Touches 1-3 files
- Has a single clear purpose
- Can be tested in isolation
- Takes roughly 20-40 minutes to implement

A segment that is too large:
- Touches more than 5 files
- Has multiple unrelated concerns
- Cannot be independently verified

**Split it.**

## Example PLAN.md structure

```markdown
# PLAN: [Ticket Title]

## Understanding

[Brief summary of what we're building and why]

## Technical Approach

[How we'll implement it, referencing existing patterns]

## Segments

### Segment 1: [Name]

**Purpose:** [Single sentence]

**Files:**
- `src/path/file1.ts` (new)
- `src/path/file2.ts` (modify)

**Definition of Done:**
- [ ] [Specific criterion 1]
- [ ] [Specific criterion 2]
- [ ] Tests pass

### Segment 2: [Name]
...

## Files Changed

- `src/auth/login.ts` (new)
- `src/middleware/auth.ts` (modify)
- `tests/auth/login.test.ts` (new)

## Out of Scope

- [Explicitly list what we're NOT building]
- [This prevents scope creep]

## Risks

- [Risk 1]: [Mitigation strategy]
- [Risk 2]: [Mitigation strategy]

## Questions for SENTINEL

- [Question 1]
- [Question 2]
```

## Contract negotiation

SENTINEL will review your plan.

If SENTINEL objects:
1. Read the objection carefully in `CONTRACT.md`
2. Update `PLAN.md` addressing each specific objection
3. Do not argue - either accept the feedback or ask a clarifying question

Maximum 3 rounds of negotiation.
After 3 rounds without agreement, the ticket is BLOCKED.

## After CONTRACT is AGREED

1. Read `CONTRACT.md` - these are your binding terms
2. Begin implementation with Segment 1
3. Do not deviate from the plan without updating `PLAN.md` and getting SENTINEL approval