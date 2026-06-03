# Contribution Guidelines

> Standards for branching, commits, pull requests, and general collaboration on OpenFlows.

---

## Table of Contents

1. [Branch Naming](#branch-naming)
2. [Commit Messages](#commit-messages)
3. [Pull Requests](#pull-requests)
4. [Issue-to-Branch Workflow](#issue-to-branch-workflow)
5. [Merging Strategy](#merging-strategy)
6. [Code Review](#code-review)
7. [General Best Practices](#general-best-practices)

---

## Branch Naming

Every branch must be tied to a GitHub issue. Use the following format:

```
<type>/<issue-number>-<short-description>
```

### Types

| Type | When to use |
|------|-------------|
| `feat` | New feature or capability |
| `fix` | Bug fix |
| `docs` | Documentation only |
| `refactor` | Code restructure, no behavior change |
| `test` | Adding or updating tests |
| `chore` | Maintenance, dependency updates, config changes |
| `ci` | CI/CD pipeline changes |

### Examples

```
feat/42-nexus-issue-prioritization
fix/87-forge-spawn-timeout
docs/103-contribution-guidelines
refactor/61-shared-store-cleanup
test/55-sentinel-review-coverage
chore/90-update-tokio-dependency
```

### Rules

- Use lowercase letters and hyphens only — no spaces, underscores, or slashes beyond the type prefix
- Keep the description short (3–5 words max)
- Always include the issue number
- Never commit directly to `main` or `develop`

---

## Commit Messages

Follow the [Conventional Commits](https://www.conventionalcommits.org/) specification.

### Format

```
<type>(<scope>): <short description>

[optional body]

[optional footer: closes #<issue-number>]
```

### Types

| Type | Purpose |
|------|---------|
| `feat` | Introduces a new feature |
| `fix` | Patches a bug |
| `docs` | Documentation changes only |
| `style` | Formatting, whitespace (no logic change) |
| `refactor` | Restructuring without behavior change |
| `test` | Adding or modifying tests |
| `chore` | Build process, tooling, dependencies |
| `ci` | CI configuration changes |
| `perf` | Performance improvements |
| `revert` | Reverts a previous commit |

### Scopes (optional but recommended)

Use the crate or module name as the scope:

```
feat(agent-nexus): add priority queue for issue assignment
fix(agent-forge): handle spawn timeout gracefully
docs(config): clarify proxy mode environment variables
```

### Examples

```
feat(agent-vessel): add conflict resolution retry logic

Closes #78
```

```
fix(agent-sentinel): prevent double review on requeued PRs

The sentinel was triggering a second review cycle when a PR was
re-added to the queue after an LLM timeout. Added a dedup check
on the PR ID before dispatching.

Closes #91
```

### Rules

- Subject line: 72 characters max, imperative mood ("add", not "added" or "adds")
- Do not end the subject line with a period
- Body: wrap at 100 characters, explain *why* not *what*
- Always reference the issue in the footer with `Closes #<number>`
- Keep commits atomic — one logical change per commit

---

## Pull Requests

### Title

Match the commit message format:

```
<type>(<scope>): <short description>
```

Examples:
```
feat(agent-nexus): add priority queue for issue assignment
fix(agent-forge): handle spawn timeout on slow providers
docs: add contribution_guidelines.md
```

### Description Template

Every PR must include:

```markdown
## What does this PR do?
<!-- One paragraph summary of the change -->

## Related Issue
Closes #<issue-number>

## Changes
<!-- Bullet list of what changed -->

## How to test
<!-- Steps to verify the change works -->

## Checklist
- [ ] Code compiles (`cargo build`)
- [ ] Tests pass (`cargo test --workspace`)
- [ ] Linter clean (`cargo clippy -- -D warnings`)
- [ ] Formatted (`cargo fmt`)
- [ ] Docs updated if needed
```

### Rules

- Link every PR to its issue (`Closes #<number>`)
- Keep PRs focused — one issue per PR
- Draft PRs are allowed for early feedback, mark them ready when complete
- PRs with failing CI will not be merged
- Do not force-push to a PR branch after review has started

---

## Issue-to-Branch Workflow

1. Pick up or get assigned to an issue on GitHub
2. Pull the latest `main`:
   ```bash
   git checkout main && git pull origin main
   ```
3. Create your branch following the naming convention:
   ```bash
   git checkout -b feat/42-your-feature-name
   ```
4. Work in focused, atomic commits
5. Push and open a PR:
   ```bash
   git push -u origin feat/42-your-feature-name
   gh pr create --fill
   ```
6. Link the PR to the issue and fill out the description template
7. Request review from at least one maintainer

---

## Merging Strategy

- The default merge method is **squash and merge** for feature and fix branches
  - This keeps `main` history clean and linear
  - The squash commit message should follow the commit format above
- **Merge commits** are used only for release branches
- **Rebase merges** are not used to avoid rewriting public history
- Delete the source branch after merging

### Protected Branches

| Branch | Rules |
|--------|-------|
| `main` | No direct pushes. PR + 1 approval + passing CI required |
| `develop` (if used) | No direct pushes. PR + passing CI required |

---

## Code Review

### As an author

- Keep PRs small and reviewable (aim for under 400 lines changed)
- Respond to all comments before requesting re-review
- Don't resolve reviewer comments yourself — let the reviewer do it
- Add inline comments to explain non-obvious decisions proactively

### As a reviewer

- Review within 1 business day of being assigned
- Distinguish blocking issues from suggestions: use `nit:` prefix for minor style notes
- Approve only when you'd be comfortable merging it yourself
- Avoid approving PRs with failing CI, even for small changes

---

## General Best Practices

### Keeping branches up to date

Rebase your branch on `main` before opening a PR:

```bash
git fetch origin
git rebase origin/main
```

Avoid long-lived branches — merge or close them within a reasonable time.

### What not to commit

- Secrets, API keys, or tokens — use `.env` (gitignored)
- Build artifacts or compiled binaries
- IDE-specific config files (`.vscode/`, `.idea/`) — add to `.gitignore`
- Large binary files — use Git LFS or external storage

### Versioning

This project follows [Semantic Versioning](https://semver.org/):

```
MAJOR.MINOR.PATCH
```

- `MAJOR` — breaking API or behavior changes
- `MINOR` — new backward-compatible features
- `PATCH` — backward-compatible bug fixes

### Changelog

Significant changes should be reflected in `CHANGELOG.md` (if present) as part of the PR.

---

> For setup instructions, architecture overview, and testing guidance, see [CONTRIBUTING.md](../CONTRIBUTING.md).
