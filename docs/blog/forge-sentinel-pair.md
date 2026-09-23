# The Forge-Sentinel Pair: Software Engineering as a Disposable Two-Player Team

> How two agents — one that builds, one that reviews — work as a 1:1 pair to ship a single
> ticket, armed with curated commands and skills, then tear themselves down when the job is
> done. Not vibes. Architecture.

---

Every software engineer knows the shape of a good workday. You don't walk in, wave your
hands, and hope the code appears. You clock in, read the ticket, think about the data flow,
write a plan, build it in small verifiable chunks, get it reviewed, and when it's merged you
pack up and go home. The workspace you occupied for those eight hours isn't sacred — it's a
disposable seat you leave behind.

The **Forge-Sentinel pair** is that workday, automated. Two agents — **FORGE**, the builder,
and **SENTINEL**, the reviewer — are provisioned as a 1:1 pair to complete exactly one task.
They are armed with a curated arsenal of commands, skills, hooks, and tools that force the
work to happen *architecturally* rather than by feel. And when the task is done, the whole
thing — workspaces, state, locks, context — is destroyed, leaving only a merged PR and a
clean slot ready for the next ticket.

This is not another "autonomous coding agent" demo. It's a discipline. Let's walk through
how the pair operates.

## One ticket, one pair, one branch

Before either agent does anything, **NEXUS** (the orchestrator) takes a GitHub issue and
assigns it to an idle pair slot. That assignment is a promise of full isolation:

- Each pair gets its own **Git worktree** on its own branch (`forge-pair-1/T-42`). Two pairs
  never share a checkout, never touch the same branch.
- Each pair gets its own **`shared/` directory** — the only channel where FORGE and SENTINEL
  talk to each other. Tickets, plans, contracts, evaluations all live there.
- Dynamic **file locks** guarantee two pairs can't stomp the same file even when scope grows.

The pair is *scoped* to the task from the moment it's born. FORGE cannot see what forge-2 is
doing. It has no reason to. Its entire universe is `TICKET.md`, its worktree, and the shared
directory where SENTINEL will reply.

## Armed with curated commands and skills — not vibes

A pair is only as reliable as its tooling. Neither agent is handed a blank chat and told
"go figure it out." Each is **provisioned with a purpose-built orchestration plugin**:
skills, hooks, slash commands, and MCP servers that encode *how* the work gets done.

### Skills — injected knowledge, not prompt bloat

On session start, each agent receives the skills relevant to its role as reference documents:

- **FORGE** loads `forge-planning` and `forge-coding` — the discipline of writing a plan
  before code, sizing segments (1–3 files, 20–40 minutes), and testing every new function.
- **SENTINEL** loads `sentinel-review` and `sentinel-criteria` — the five evaluation
  criteria (correctness, test coverage, standards compliance, code quality, no regressions)
  that every approval must pass.

The skills don't make the agents *capable* — they make them *consistent*. FORGE doesn't
wonder whether it should test; the skill says every new function needs a test, and the hook
enforces it.

### Hooks — invariants the agent cannot bypass

Skills are suggestions; hooks are law. Lifecycle hooks run around every action and refuse
to let the agent skip the process:

- **FORGE**: a `pre_write_check` hook enforces file ownership locks before every write; a
  `post_write_lint` hook runs the linter on every changed file; a `pre_bash_guard` blocks
  dangerous commands like direct `git push`; a `stop_require_artifact` hook **refuses to let
  FORGE exit** until it has produced a valid `STATUS.json` or `HANDOFF.md`.
- **SENTINEL**: a `pre_bash_readonly_guard` makes the reviewer read-only against source —
  it can run tests and linters but cannot modify a single line of FORGE's code; a
  `stop_require_eval` hook refuses exit until an evaluation is written.

The message is blunt: *you don't get to leave until the work is finished and recorded.*
That's the equivalent of an engineer being told the ticket isn't done until the PR is up and
the tests are green.

### Commands — structured procedures

Slash commands bundle multi-step procedures so the agent doesn't improvise the workflow:

- `/plan` — reads the ticket, searches the codebase for existing patterns, and writes a
  structured `PLAN.md` with segment breakdown and definition of done.
- `/segment-done` — runs the full test suite, lints changed files, commits the segment, and
  notifies SENTINEL.
- `/handoff` — writes a complete handoff and exits cleanly when context runs low.
- `/status` — verifies the final review is approved, pushes the branch, opens the PR via the
  GitHub MCP server, and records the terminal `STATUS.json`.

### MCP servers — external capability, scoped

Rather than build a custom tool binary, the pair is wired to **battle-tested MCP servers** —
GitHub, Redis, filesystem, shell — each scoped to the pair's own worktree and credentials.
FORGE can open a PR through GitHub MCP; the shell server only allows an explicit allowlist
(`run-tests.sh`, `cargo test`, `eslint`, `ruff`). The agent gets real capability with
constraints that make it safe to run unattended.

## The gated workflow: planning → building → reviewing

The pair's rhythm is a **series of hard gates**. FORGE cannot skip ahead, and SENTINEL
cannot approve without evidence.

### 1. The planning gate

FORGE reads the ticket, searches the codebase for existing patterns, and writes `PLAN.md`:
its understanding of the ticket, the technical approach, an explicit segment breakdown, a
testable definition of done per segment, and an out-of-scope list. It then sets
`status set planning` and **halts**.

The harness watches the shared directory and, on seeing the plan, spawns a fresh SENTINEL
to review it *before a single line of code is written*. SENTINEL checks whether the plan
addresses every acceptance criterion, whether the approach follows the codebase's
architecture patterns, and whether the definition of done is actually testable. It writes
`CONTRACT.md` — either `AGREED` or a list of specific objections.

If there are objections, the plan goes back. After **three rounds** of disagreement, the
ticket is blocked and escalated. No code was written during any of this — the architecture
was agreed *first*. This is the pair's refusal to build on vibes.

### 2. The segment loop

Only after gate approval does FORGE set `status set building` and begin implementation —
in small, independently verifiable segments. After each segment:

1. FORGE runs the full test suite — it must pass.
2. FORGE runs the linter — zero warnings.
3. FORGE commits and signals `/segment-done`.
4. The harness spawns a **fresh, ephemeral SENTINEL** to evaluate *that one segment*.

SENTINEL runs the tests, lints the changed files, reads every changed file against the
contract, and evaluates against the five criteria. Feedback is never vague — every finding
is `file:line:problem:fix`, e.g.:

> `src/auth/login.ts:23`: Missing error handling for `fetchUser()`. Required: Add try-catch
> with `AppError('USER_NOT_FOUND', 404)`.

If SENTINEL requests changes, FORGE fixes *exactly those items* — not a refactor — and
re-submits. Each re-submission spawns a fresh SENTINEL with zero memory of the previous one.
That freshness is deliberate.

### 3. Final review and the PR

When every segment is approved, SENTINEL spawns once more for a **final review**: the full
test suite, the full linter, every contract criterion. On approval it writes
`final-review.md` with an `APPROVED` verdict and — importantly — the PR description.

FORGE then pushes the branch and opens the PR through GitHub MCP, using SENTINEL's own
write-up as the body. It records `STATUS.json` and exits. **VESSEL** — a separate,
deterministic agent — checks CI and merges. The pair's job is done.

## The destroy: ephemeral by design

Here is where the pair departs from most "agent" systems and starts to resemble a real
engineer's workday: **everything is disposable, and everything is destroyed when the task is
done.**

- **SENTINEL is not a long-running process.** It is spawned fresh for *every* evaluation —
  plan review, each segment, the final review — and it exits as soon as it has written its
  verdict. It has no history, no future, only the one segment in front of it. This guarantees
  **zero context drift**: the reviewer of segment 3 has no memory or bias from segment 2. It
  cannot be talked into approving something it saw "last time," because there was no last time.
- **The worker workspace is disposable.** Each pair runs in a short-lived Coder workspace
  that holds no credentials, no LLM keys, and no agent framework. The workspace is a *seat*,
  not a home.
- **On completion, the harness tears the pair down.** The worktree is removed and recreated
  idle on `main`. File locks owned by the pair are released. The `shared/` artifacts that
  carried the whole conversation — plans, contracts, evaluations, status — are runtime state,
  explicitly gitignored, never committed. The slot returns to `IDLE`, ready for the next
  ticket.

There is no "agent garden" of long-lived reviewers accumulating context and ego. There is a
slot that gets provisioned, does its eight-hour job, and gets reset.

## Why the destroy matters

Ephemerality isn't just convenient — it's the source of the system's trustworthiness. Three
consequences fall out of it:

1. **No context drift.** A reviewer that has seen 40 PRs has opinions. A reviewer that is
   born, evaluates one thing, and dies has only the evidence in front of it. The pair's
   quality bar is constant because the reviewer is always fresh.
2. **No state rot.** Long-running agents accumulate stale assumptions, half-finished
   workflows, and locked files. The pair's teardown sweeps all of it away. An idle slot is a
   *guaranteed-clean* slot.
3. **Isolation is real.** Because FORGE and SENTINEL live in separate, disposable
   workspaces, adversarial review is honest: SENTINEL can run FORGE's tests but can never
   touch FORGE's filesystem. It either sees evidence, or it blocks.

## Architecture over vibe

The contrast the pair is built around is the difference between an engineer who "feels" the
feature is done and one who *knows* it is:

- **Plan before code** — the architecture is agreed and written down before implementation.
- **Small verifiable segments** — each one tested, linted, and reviewed on its own.
- **Machine-readable handshake** — the controller consumes a harness command's Redis write,
  not a `STATUS.json` file the agent might forget. The verdict is recorded by a command, not
  by hope.
- **Hard-fail on missing evidence** — SENTINEL must *hard-fail, never approve*, when a
  required artifact is missing or unreadable. Unknown ≠ pass.
- **No partial credit** — a PR either earns its merge against the contract, or it goes back.

That is an eight-hour shift in miniature: clock in, read the ticket, plan, build in reviewable
chunks, get it reviewed, ship it, and tear down the seat you occupied. The Forge-Sentinel
pair is what it looks like when software engineering stops being a vibe and becomes a
**discipline with a teardown clause** — and the only thing left behind is the merged feature.
