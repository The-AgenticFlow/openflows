---
name: sentinel-criteria
description: Evaluation criteria for the SENTINEL reviewer agent
---

# SENTINEL Evaluation Criteria

## The five criteria - all must pass for any approval

### 1. Correctness

Does the implementation correctly handle all cases described in CONTRACT.md?
Does it handle the error paths, edge cases, and boundary conditions?

**FAIL:** any CONTRACT criterion not met, any obvious logic error

### 2. Test coverage

Are all changed files covered by tests?
Does every new function have at least one test for the happy path
and one for the primary error path?

**FAIL:** any changed file with no tests, any new function with no test

### 3. Standards compliance

Does the implementation follow `orchestration/agent/standards/CODING.md`?
Does it use the patterns in `orchestration/agent/arch/patterns.md`?
Does it respect the API contracts in `orchestration/agent/arch/api-contracts.md`?

**FAIL:** any violation of the team's written standards

### 4. Code quality

Is the code readable? Are names clear? Is complexity justified?
Is there duplication that should be extracted?

**NOTE:** This criterion is advisory - it cannot block alone.
It informs feedback but a single quality concern is not a blocker.

### 5. No regressions

Do all existing tests still pass?
Has any existing behaviour been changed without explicit ticket scope?

**FAIL:** any previously passing test now failing

---

## Evaluation checklist

Use this checklist for every segment review:

```markdown
## Evaluation Checklist

### Correctness
- [ ] All CONTRACT criteria verified
- [ ] Error paths handled
- [ ] Edge cases covered
- [ ] Boundary conditions tested

### Test Coverage
- [ ] All changed files have tests
- [ ] New functions have happy path tests
- [ ] New functions have error path tests
- [ ] Integration tests where needed

### Standards Compliance
- [ ] CODING.md rules followed
- [ ] patterns.md patterns used
- [ ] API contracts respected
- [ ] Naming conventions followed

### Code Quality
- [ ] Readable and clear
- [ ] No unnecessary duplication
- [ ] Appropriate abstraction level
- [ ] Comments where needed

### No Regressions
- [ ] All existing tests pass
- [ ] No unintended behavior changes
```

---

## Severity levels

When writing feedback, indicate severity:

- **BLOCKER:** Must be fixed before approval (correctness, test coverage, standards)
- **MAJOR:** Should be fixed, but approval possible if addressed in next segment
- **MINOR:** Advisory, can be deferred

### Example with severity

```markdown
## Specific feedback

- `src/auth/login.ts:23` [BLOCKER]: Missing error handling for `fetchUser()`. Required: Add try-catch with `AppError('USER_NOT_FOUND', 404)`

- `src/auth/login.ts:67` [MAJOR]: Hardcoded timeout value. Required: Use `config.timeout` from `src/config.ts`

- `src/auth/login.ts:89` [MINOR]: Variable name `tmp` is unclear. Consider: `userSession`
```

---

## Quick decision guide

| Situation | Verdict |
|-----------|---------|
| All criteria pass | APPROVED |
| Any BLOCKER items | CHANGES_REQUESTED |
| Multiple MAJOR items | CHANGES_REQUESTED |
| Only MINOR items | APPROVED (with notes) |
| Tests failing | CHANGES_REQUESTED |
| Lint warnings | CHANGES_REQUESTED |
| Standards violation | CHANGES_REQUESTED |