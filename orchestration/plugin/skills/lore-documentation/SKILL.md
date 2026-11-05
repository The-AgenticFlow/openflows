---
name: documentation
description: Skill for maintaining project documentation
---

# Documentation Skill

## Documentation Types

### User Documentation
- `README.md`: Project overview, setup, usage
- `docs/usage/`: How-to guides
- `docs/api/`: API reference

### Developer Documentation
- `CONTRIBUTING.md`: Contribution guidelines
- `docs/architecture/`: System design
- `.agent/standards/`: Coding standards

### Changelog
- `CHANGELOG.md`: User-facing changes

## When to Update

Update documentation when:
- New feature added
- Breaking change introduced
- API modified
- Configuration changed
- Dependencies updated

## Documentation Style

### README Updates
- Keep concise
- Update examples
- Reflect current API

### API Documentation
- Document all public interfaces
- Include examples
- Document error conditions

### Architecture Docs
- Update diagrams if structure changed
- Document new patterns
- Update decision records

## Commit Format

```
docs: update README for new auth feature

- Add OAuth2 setup instructions
- Update configuration example
- Add troubleshooting section
```

## Delivery Gate (Coder workspace)

NEXUS starts a LORE chat for each merged ticket. The docs record is the
authority; read it with `openflows-harness docs status`.

1. Read `openflows-harness dispatch read` for the merged ticket and PR. The
   dispatch `branch` is the docs branch you must use (`lore/<ticket>`, or
   `lore/<ticket>--c<N>` after a reopen).
2. If that branch exists on origin, check it out, pull it, and continue from
   it; otherwise create it from the default branch. Change only documentation:
   Markdown, reST and AsciiDoc files, changelogs, and text files and images
   under `docs/`. Never touch dot-directories or dotfiles (`.github/`,
   `.cursor/`, `.changeset/`, ...), `orchestration/`, agent instructions
   (`*.agent.md`, `SKILL.md`, `CLAUDE.md`, `AGENTS.md`), legal files
   (`LICENSE`, `NOTICE`, `CODEOWNERS`), MDX, or code, config and build-input
   files such as `requirements.txt` (even under `docs/`); `docs submit`
   refuses them, renames included, and the controller re-checks the pushed
   head on GitHub before anything is published.
3. Commit, push the branch to origin, write a summary to
   `/tmp/docs-report.md` (outside the repository, so the checkout stays clean),
   and run `openflows-harness docs submit --report /tmp/docs-report.md`.
4. Stop. The operator approves or rejects that exact revision and head with
   `openflows gate docs`, or pauses, restarts or reopens LORE. A rejection
   returns feedback; revise, push, and submit again.
5. Never open a PR. After approval the controller opens the docs PR and VESSEL
   merges it at the approved head. If the docs conflict with the default
   branch, fail CI, change non-documentation files, or the branch moved, you
   get the reason as feedback: fix
   it on the same branch (for conflicts, merge `origin/<default>` and resolve
   keeping documentation only), push, and submit again.

Commands are refused while paused or from any role but LORE. Follow the
guidance in each refusal instead of retrying blindly.
