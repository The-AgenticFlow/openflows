# Changelog

All notable changes to this project will be documented in this file.
<!-- markdownlint-disable line-length no-bare-urls ul-style emphasis-style -->

## [1.5.8] - 2026-09-22

### Bug Fixes

- *(docker)* Replace unreliable workflow_run trigger and eliminate QEMU bottleneck







  by @test

### Documentation

- *(manager)* Comment service bootstrap and health paths







  by @NkwaTambe




### Contributors

- @NkwaTambe
- @test

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.7...openflows-1.5.8



## [1.5.7] - 2026-09-22

### Features

- *(uncategorized)* Scaffold openflows manager service







  by @NkwaTambe

### Bug Fixes

- *(manager)* Bound readiness check duration







  by @NkwaTambe

- *(uncategorized)* Harden manager readiness and shutdown







  by @NkwaTambe



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.6...openflows-1.5.7



## [1.5.5] - 2026-09-22

### Features

- *(bootstrap)* Remove vestigial admin-owned nexus workspace ([#205](https://github.com/The-AgenticFlow/openflows/pull/205))







  by @NkwaTambe

### Bug Fixes

- *(bootstrap)* Provision tenant workspace under admin session user






  by @Christiantyemele

- *(bootstrap)* Refuse admin identity when tenant user cannot be resolved






  by @Christiantyemele

- *(docker-compose)* Add TF_PLUGIN_CACHE_DIR for shared Terraform provider caching






  by @Christiantyemele

- *(external-auth)* Retrieve device flow via GET not POST






  by @Christiantyemele

- *(hooks)* Block quoted shell-wrapper lifecycle calls (review)






  by @Christiantyemele

- *(hooks)* Anchor coder lifecycle guard to real invocations






  by @Christiantyemele

- *(nexus)* Preserve assigned fallback sentinel as review owner






  by @Christiantyemele

- *(nexus)* Enforce strict 1:1 sentinel pairing per forge slot






  by @Christiantyemele

- *(nexus)* Do not grant chat ownership on transient lookup failure






  by @Christiantyemele

- *(nexus)* Enforce single-owner fallback on shared sentinel chat






  by @Christiantyemele

- *(nexus)* Allow fallback/assigned sentinel to own shared chat during recovery






  by @Christiantyemele

- *(nexus)* Enforce 1:1 forge-sentinel fleet pairing and parallel spawn






  by @Christiantyemele

- *(resolve-version)* Bound window to exclude later release tags







  by @Christian Yemele

- *(scripts)* Add 'run' to reset-script controller instruction






  by @Christiantyemele

- *(templates)* Export external-auth token into workspace env






  by @Christiantyemele

### Refactor

- *(auth)* Use GitHub App external auth as the sole GitHub token source






  by @Christiantyemele

- *(run)* Remove duplicate host-side controller run command






  by @Christiantyemele

### Documentation

- *(quickstart)* Align OAuth link and secret rotation with tenant-owned workspaces (review)







  by @NkwaTambe

### Styling

- *(format)* Apply cargo fmt across changed files






  by @Christiantyemele

### Miscellaneous Tasks

- *(docs)* Clean up .env.example by removing optional admin user configuration






  by @Christiantyemele

- *(format)* Improve code readability by adjusting formatting in bootstrap.rs






  by @Christiantyemele



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.4...openflows-1.5.5



## [1.5.4] - 2026-09-18

### Bug Fixes

- *(resolve-version)* Drop end_at upper bound to fix clock-skew tag miss






  by @Christiantyemele

- *(resolve-version)* Add grace period to end_at timestamp for clock skew






  by @Christiantyemele

- *(resolve-version)* Drop end_at upper bound to fix clock-skew tag miss






  by @Christiantyemele



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.3...openflows-1.5.4



## [1.5.3] - 2026-09-17

### Bug Fixes

- *(changelog)* Stop release-plz crashing on placeholder commit id






  by @Christiantyemele

- *(changelog)* Render valid GitHub mentions from commit author






  by @Christiantyemele

- *(docker)* Copy workspace source before cargo fetch






  by @Christiantyemele



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.2...openflows-1.5.3



## [1.5.2] - 2026-09-17

### Features

- *(env)* Make tenant repo the single source of truth ([#213](https://github.com/The-AgenticFlow/openflows/pull/213))







  by @NkwaTambe

### Bug Fixes

- *(bootstrap)* Warn when existing tenant workspace needs recreation







  by @NkwaTambe

- *(ci,tenant)* Address review — resolver, tenant repo persist, owner-scoped warn






  by @Christiantyemele

- *(controller)* Fail fast when no target repository can be resolved







  by @NkwaTambe

### Documentation

- *(arch)* Describe current repo source without past-comparison (review)







  by @NkwaTambe

- *(uncategorized)* Soften multi-tenant claim and reduce DEV/DEBUG repetition (review)







  by @NkwaTambe



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.1...openflows-1.5.2



## [1.5.0] - 2026-09-17

### Features

- *(debug)* Add workspace provider function with fallback for debug output






  by @Christiantyemele

### Bug Fixes

- *(changelog)* Render clean commit lines without links or bodies






  by @Christiantyemele

### Continuous Integration

- *(uncategorized)* Fix zizmor workflow_run check on docker-publish






  by @Christiantyemele



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.4.0...openflows-1.5.0



## [1.4.0] - 2026-09-17

### Features

- *(debug)* Surface compiled binary version in debug output







  by @v



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.3.3...openflows-1.4.0



## [1.3.3] - 2026-09-17

### Documentation

- *(readme)* Add GHCR image link







  by @v



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.3.2...openflows-1.3.3



## [1.3.0] - 2026-09-16

### Features

- *(coder)* Pin Coder to v2.37.0 and validate version in doctor ([#186](https://github.com/The-AgenticFlow/openflows/pull/186))







  by @AgentFlow

- *(coder-client)* Migrate Chats API to /api/v2 ([#186](https://github.com/The-AgenticFlow/openflows/pull/186))







  by @AgentFlow

- *(config)* Centralize remaining env reads incl GITHUB_API_BASE (#185, #204)







  by @AgentFlow

- *(config)* Centralize environment configuration with envconfig ([#185](https://github.com/The-AgenticFlow/openflows/pull/185))







  by @AgentFlow

- *(docker)* Update Coder image version and add lifecycle hook configuration







  by @AgentFlow

- *(hooks)* Implement hook-driven state derivation and behavior triggering







  by @AgentFlow

- *(hooks)* Add standalone hook consumer for local testing







  by @AgentFlow

- *(hooks)* Consume Coder agent-lifecycle-hooks (experimental)







  by @AgentFlow

- *(hygiene)* Remove migration doc






  by @Christiantyemele

- *(uncategorized)* Prefer GitHub PAT for workspace git, fix repo acquisition







  by @AgentFlow

### Bug Fixes

- *(coder)* Use Coder external-auth token for workspace git access







  by @NkwaTambe

- *(coder)* Bump image tag default to v2.37.1 for GitHub App auth (fixes #211)







  by @NkwaTambe

- *(coder-client)* Reject non-stream paths in mock chat server







  by @AgentFlow

- *(coder-client)* Use correct chat stream endpoint and resolve org earlier







  by @AgentFlow

- *(config)* Address PR review feedback on env centralization







  by @AgentFlow

- *(config)* Address review feedback on env centralization ([#204](https://github.com/The-AgenticFlow/openflows/pull/204))







  by @NkwaTambe

- *(config)* Address PR review feedback ([#204](https://github.com/The-AgenticFlow/openflows/pull/204))







  by @AgentFlow

- *(doctor)* Require external-auth client ID; centralize via CoderConfig ([#219](https://github.com/The-AgenticFlow/openflows/pull/219))







  by @NkwaTambe

- *(doctor)* Only compare semantic-version Coder tags, skip floating tags







  by @AgentFlow

- *(doctor, bootstrap)* Note external auth as needed for agent auth







  by @NkwaTambe

- *(hooks)* Close argv probe bypass and wrapper-argument write evasion







  by @AgentFlow

- *(hooks)* Close blocked-probe write bypass and RUSTSEC-2023-0071







  by @AgentFlow

- *(hooks)* Harden guard against wrapped/segmented write evasion and compound replan bypass







  by @AgentFlow

- *(hooks)* Harden phase guard against write/phase bypasses







  by @AgentFlow

- *(hooks)* Propagate OPENFLOWS_HOOK_URL to consumer and document workspace recreation







  by @AgentFlow

- *(hooks)* Make CODER_CHAT_HOOK_URL the single source of truth for aud







  by @AgentFlow

- *(hooks)* Classify shell writes by actual target, not artifact name







  by @AgentFlow

- *(hooks)* Harden lifecycle hooks per review feedback







  by @AgentFlow

- *(hooks)* Accept Coder's deployment-ID issuer in hook JWT







  by @AgentFlow

- *(lifecycle-hooks)* Recover hook orchestration and replay pending events







  by @AgentFlow

- *(nexus)* Default coder_url to localhost:7080 in coder_client_from_store







  by @NkwaTambe

- *(startup)* Address review findings on cleanup, count, and worker auth







  by @NkwaTambe

- *(startup)* Simplify quick-start and fix onboarding bugs







  by @NkwaTambe

- *(uncategorized)* Derive release version safely on manual dispatch of release-assets







  by @AgentFlow

### Documentation

- *(architecture)* Record Coder v2.37.0 GA migration findings







  by @NkwaTambe

- *(coder)* Document git-credential store tradeoff vs GIT_ASKPASS ([#220](https://github.com/The-AgenticFlow/openflows/pull/220))







  by @NkwaTambe

- *(config)* Update env audit to match external auth field names ([#204](https://github.com/The-AgenticFlow/openflows/pull/204))







  by @AgentFlow

- *(quick-start)* Focus guide on operator setup for hooks and GitHub App







  by @AgentFlow

- *(quick-start)* Address Greptile review on GitHub App setup







  by @NkwaTambe

- *(quick-start)* Document GitHub App permissions and installation-approval







  by @NkwaTambe

- *(quick-start)* Document linking the GitHub App for private-repo access







  by @NkwaTambe

- *(quick-start)* Verify GitHub App via deployment external-auth page after sign-in







  by @NkwaTambe

- *(quick-start)* External auth block is already uncommented; just set values







  by @NkwaTambe

- *(quick-start)* Simplify install URL to concrete example







  by @NkwaTambe

- *(quick-start)* Explain how to find the GitHub App slug







  by @NkwaTambe

- *(quick-start)* Point to the redirect URI field as the callback







  by @NkwaTambe

- *(quick-start)* Drop GitHub App nav path; clarify Homepage URL field







  by @NkwaTambe

- *(quick-start)* Rewrite quickstart with GitHub App auth; lowercase all .md docs







  by @NkwaTambe

- *(uncategorized)* Address review — fix WS stream endpoint, pinning and §8.4 ref







  by @AgentFlow

- *(uncategorized)* Repair dangling links after architecture doc restructure







  by @AgentFlow

- *(uncategorized)* Restructure architecture docs and add GitHub external auth config







  by @AgentFlow

### Styling

- *(doctor)* Wrap long println to satisfy rustfmt







  by @NkwaTambe

- *(uncategorized)* Apply rustfmt formatting







  by @AgentFlow

### Testing

- *(coder-client)* Cover v2 chat paths ([#186](https://github.com/The-AgenticFlow/openflows/pull/186))







  by @AgentFlow

### Miscellaneous Tasks

- *(coder)* Update non-code refs to org-scoped v2 models ([#186](https://github.com/The-AgenticFlow/openflows/pull/186))







  by @AgentFlow

- *(uncategorized)* Finalize single-bundle release from develop to main







  by @AgentFlow

- *(uncategorized)* Revert to single openflows release without orchestration bundling







  by @AgentFlow



**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-harness-1.2.1...openflows-1.3.0



## [harness-1.2.1] - 2026-08-24

### Features

- *(lore)* Commit and push docs PR after generating documentation







  by @Christian Yemele

- *(lore)* Commit and push docs PR after generating documentation







  by @Christian Yemele

- *(model-discovery)* Dynamically discover Anthropic models from API







  by @AgentFlow

- *(nexus)* Add real E2E test, mcp-proxy bridge, and enhanced logging







  by @Christian Yemele

- *(nexus)* Add real E2E test, mcp-proxy bridge, and enhanced logging







  by @Christian Yemele

- *(phase2)* Implemented nexus







  by @Christian Yemele

- *(phase2)* Implemented nexus







  by @Christian Yemele

- *(setup)* Auto-detect and set CLI binary path in .env







  by @AgentFlow

- *(tui)* Redesign agent config screen with inline editing







  by @AgentFlow

- *(tui)* Add agent configuration step for instances, model backend, and tokens







  by @AgentFlow

- *(uncategorized)* Split releases into openflows and openflows-harness without v prefix







  by @AgentFlow

- *(uncategorized)* Run releases only on pushes to main







  by @AgentFlow

- *(uncategorized)* Auto-version releases from conventional commits







  by @AgentFlow

- *(uncategorized)* Adopt release-plz with develop-as-default branching







  by @AgentFlow

- *(uncategorized)* Openflows x coder







  by @AgentFlow

- *(uncategorized)* Add tenant clean command and gate approval system







  by @AgentFlow

- *(uncategorized)* Add dev-sync script and auto-sync binary in bootstrap







  by @AgentFlow

- *(uncategorized)* Include full agent persona in initial chat prompt







  by @AgentFlow

- *(uncategorized)* Orchestrator Coder agent integration fixes







  by @AgentFlow

- *(uncategorized)* Implement Coder integration plan (Phases 1-3)







  by @AgentFlow

- *(uncategorized)* Include issue number in forge PR prompts to link pull requests automatically







  by @AgentFlow

- *(uncategorized)* Embed all orchestration files at compile time with self-healing and version tracking







  by @AgentFlow

- *(uncategorized)* Auto-release on push to main with stable/edge channels







  by @AgentFlow

- *(uncategorized)* Add domain configuration step to interactive setup







  by @AgentFlow

- *(uncategorized)* Add detailed prerequisites to README, refactor output indexing in responses proxy, extract strip_provider_prefix to agent-client







  by @AgentFlow

- *(uncategorized)* Packaging and distribution setup







  by @AgentFlow

- *(uncategorized)* Separate GitHub and LLM provider setup into distinct steps







  by @AgentFlow

- *(uncategorized)* Update GitHub username handling for issue assignment and add authenticated user retrieval







  by @Ngha-Boris

- *(uncategorized)* Implement GitHub issue assignment, comment, and labeling functionality







  by @Ngha-Boris

- *(uncategorized)* Add DEFAULT_CLI env var override for CLI backend selection







  by @ndefokou

- *(uncategorized)* Per-agent GitHub tokens with instance suffix support







  by @AgentFlow

- *(uncategorized)* Per-agent GitHub tokens, git identity from PAT, and REST client extensions







  by @AgentFlow

- *(uncategorized)* Auto-sync worktree branches with main on merge







  by @Christian Yemele

- *(uncategorized)* Add conflict resolver, enhance vessel/nexus agents, and improve proxy-first routing







  by @Christian Yemele

- *(uncategorized)* Implement VESSEL agent (DevOps & Merge Gate)







  by @Christian Yemele

- *(uncategorized)* Add Docker infrastructure with LiteLLM proxy and agent-team service







  by @Christian Yemele

- *(uncategorized)* Add model mapping and streaming support to anthropic-proxy







  by @Christian Yemele

- *(uncategorized)* Complete FORGE-SENTINEL pair lifecycle with SENTINEL review certification







  by @Christian Yemele

- *(uncategorized)* Switch real_test to use ForgePairNode for full SENTINEL lifecycle







  by @Christian Yemele

- *(uncategorized)* Implement FORGE-SENTINEL pair architecture v3.0







  by @Christian Yemele

- *(uncategorized)* Complete Phase 4 orchestration with robust JSON extraction and worker logging







  by @Christian Yemele

- *(uncategorized)* Implement hosted MCP bridge, nexus E2E tests, and contributing guide







  by @Christian Yemele

- *(uncategorized)* Per-agent GitHub tokens with instance suffix support







  by @AgentFlow

- *(uncategorized)* Per-agent GitHub tokens, git identity from PAT, and REST client extensions







  by @AgentFlow

- *(uncategorized)* Auto-sync worktree branches with main on merge







  by @Christian Yemele

- *(uncategorized)* Add conflict resolver, enhance vessel/nexus agents, and improve proxy-first routing







  by @Christian Yemele

- *(uncategorized)* Implement VESSEL agent (DevOps & Merge Gate)







  by @Christian Yemele

- *(uncategorized)* Add Docker infrastructure with LiteLLM proxy and agent-team service







  by @Christian Yemele

- *(uncategorized)* Add model mapping and streaming support to anthropic-proxy







  by @Christian Yemele

- *(uncategorized)* Complete FORGE-SENTINEL pair lifecycle with SENTINEL review certification







  by @Christian Yemele

- *(uncategorized)* Switch real_test to use ForgePairNode for full SENTINEL lifecycle







  by @Christian Yemele

- *(uncategorized)* Implement FORGE-SENTINEL pair architecture v3.0







  by @Christian Yemele

- *(uncategorized)* Complete Phase 4 orchestration with robust JSON extraction and worker logging







  by @Christian Yemele

- *(uncategorized)* Implement hosted MCP bridge, nexus E2E tests, and contributing guide







  by @Christian Yemele

### Bug Fixes

- *(a2a)* Add pair-scope ownership check to tasks/cancel







  by @AgentFlow

- *(a2a)* Complete verify task transport end-to-end







  by @AgentFlow

- *(a2a-protocol)* Declare MIT license (fixes cargo-deny license check)







  by @AgentFlow

- *(agent-forge)* Address PR review edge cases in denylist and validation







  by @AgentFlow

- *(agent-forge)* Address PR review feedback on configuration leak prevention







  by @AgentFlow

- *(agent-forge)* Prevent sensitive agent configuration leaks







  by @AgentFlow

- *(deny)* Add Zlib license to allow list (used by foldhash dependency)







  by @AgentFlow

- *(deny)* Allow NOASSERTION license for workspace crates







  by @AgentFlow

- *(deny)* Add license exception for agentflow-tui workspace crate







  by @AgentFlow

- *(deny)* Remove invalid clarify section, keep private.ignore







  by @AgentFlow

- *(deny)* Remove deprecated unlicensed key, use private.ignore







  by @AgentFlow

- *(deny)* Set unlicensed=allow for workspace crates without license files







  by @AgentFlow

- *(deny)* Ignore private workspace crates in license check







  by @AgentFlow

- *(docker)* Copy orchestration/ directory in build stage







  by @AgentFlow

- *(docs)* Enhance bootstrap instructions with default admin credentials and password requirements







  by @Ngha-Boris

- *(docs)* Update Step 4 in QUICK_START for clarity on Coder sign-in and API token retrieval







  by @Ngha-Boris

- *(docs)* Update QUICK_START and README for clarity and structure







  by @Ngha-Boris

- *(docs)* Clarify controller log verification process in README







  by @Ngha-Boris

- *(docs)* Add optional Coder admin user configuration and clarify tenant command syntax in README







  by @Ngha-Boris

- *(docs)* Update Coder session token placeholder in .env.example for clarity







  by @Ngha-Boris

- *(docs)* Update tenant command syntax and improve log verification instructions in README







  by @Ngha-Boris

- *(docs)* Update API token instructions and clarify Coder license creation process in README







  by @Ngha-Boris

- *(docs)* Clarify instructions for copying the API token in README







  by @Ngha-Boris

- *(docs)* Update environment configuration in .env.example and docker-compose.yml for clarity







  by @Ngha-Boris

- *(docs)* Update Docker command in README to run in detached mode and clarify log monitoring instructions







  by @Ngha-Boris

- *(docs)* Update Docker command in README for clarity







  by @Ngha-Boris

- *(fallback)* Remove Gemini from provider chain, keep only 3 supported providers







  by @AgentFlow

- *(forge)* Handle STATUS.json parsing and success outcomes







  by @Christian Yemele

- *(forge)* Claude Code stdin prompt and workspace isolation







  by @Christian Yemele

- *(forge)* Handle STATUS.json parsing and success outcomes







  by @Christian Yemele

- *(forge)* Claude Code stdin prompt and workspace isolation







  by @Christian Yemele

- *(install)* Skip harness when no release exists in the selected channel







  by @AgentFlow

- *(install)* Honor musl fallback extraction dir and drop invalid gh --arg queries







  by @AgentFlow

- *(install)* Resolve release tags per package and install harness from its own archive







  by @AgentFlow

- *(install)* Check for either Claude Code or Codex, don't auto-install







  by @AgentFlow

- *(lore)* Branch docs PRs from origin/main instead of forge branch







  by @AgentFlow

- *(lore)* Route merged PRs to lore before CI fixes







  by @Christian Yemele

- *(lore)* Branch docs PRs from origin/main instead of forge branch







  by @AgentFlow

- *(lore)* Route merged PRs to lore before CI fixes







  by @Christian Yemele

- *(mcp)* Inject BEARER_TOKEN via Command::env() to fix Bearer prefix







  by @Christian Yemele

- *(mcp)* Inject BEARER_TOKEN via Command::env() to fix Bearer prefix







  by @Christian Yemele

- *(model-discovery)* Validate Anthropic model IDs, reject invalid API names







  by @AgentFlow

- *(model-discovery)* Use verified working Anthropic model IDs







  by @AgentFlow

- *(model-discovery)* Use valid Anthropic API model IDs, not CLI aliases







  by @AgentFlow

- *(nexus)* Detect and re-spawn orphaned sentinel chats







  by @AgentFlow

- *(nexus)* Move FORGE planning-gate notification into polling loop







  by @AgentFlow

- *(nexus)* Ensure sentinel spawns for planning gates, add forge gate-wait protocol







  by @AgentFlow

- *(nexus)* Rotate forge chats bound to stale/dead workspaces







  by @AgentFlow

- *(orchestration)* Check Assigned tickets for planning gate and review_ready







  by @AgentFlow

- *(orchestration)* Spawn SENTINEL for planning-gate review







  by @AgentFlow

- *(pair-harness)* Add ticket_id field to PairConfig for per-issue scoping







  by @Christian Yemele

- *(pair-harness)* Add ticket_id field to PairConfig for per-issue scoping







  by @Christian Yemele

- *(proxy)* Add /v1/models endpoint for Claude Code model validation







  by @AgentFlow

- *(sentinel)* Consume review verdict after processing, fix forge chat lookup







  by @AgentFlow

- *(setup)* Ensure CLI backend is properly set in registry.json







  by @AgentFlow

- *(setup)* Add GITHUB_MCP_CMD to .env file







  by @AgentFlow

- *(templates)* Escape ${HARNESS_ASSET} in startup heredoc so Terraform plans







  by @AgentFlow

- *(templates)* Make workspace harness install robust; fix repo URL







  by @AgentFlow

- *(templates)* Wire workspaces to the nexus A2A relay







  by @AgentFlow

- *(templates)* Correct $$ escaping in sentinel/lore startup scripts







  by @AgentFlow

- *(templates/sentinel)* Add shared orchestration volume mount







  by @AgentFlow

- *(templates/sentinel)* Wire coder_parameter blocks (ticket_id, tenant, coder_url)







  by @AgentFlow

- *(tui)* Improve setup flow order and add edit option







  by @AgentFlow

- *(tui)* Lock Nexus instances, fix GitHub token propagation, anchor artifacts to ~/.agentflow







  by @AgentFlow

- *(tui)* Make current_state mutable in agent step







  by @AgentFlow

- *(vessel)* Detect merge conflicts in merge result and route to conflict handler







  by @AgentFlow

- *(vessel)* Detect merge conflicts in merge result and route to conflict handler







  by @AgentFlow

- *(uncategorized)* Verify zig toolchain SHA-256 on install







  by @AgentFlow

- *(uncategorized)* Report guard check as validate-release-branch to match main protection







  by @AgentFlow

- *(uncategorized)* Authenticate build toolchain and enforce release branch shape







  by @AgentFlow

- *(uncategorized)* Address gitar PR review on release workflows







  by @AgentFlow

- *(uncategorized)* Harden release-source provenance and main scoping







  by @AgentFlow

- *(uncategorized)* Scope workflow permissions to job level for zizmor







  by @AgentFlow

- *(uncategorized)* Avoid shell injection in PR title lint step







  by @AgentFlow

- *(uncategorized)* Address review feedback on title lint and manual release







  by @AgentFlow

- *(uncategorized)* Use ESM export in commitlint config







  by @AgentFlow

- *(uncategorized)* Scope release-plz packages and repair CI workflows







  by @AgentFlow

- *(uncategorized)* Scope workflow permissions to job level for zizmor







  by @AgentFlow

- *(uncategorized)* Bump h2 to 0.4.16 to fix RUSTSEC-2026-0258







  by @AgentFlow

- *(uncategorized)* Reuse existing CODER_SESSION_TOKEN instead of hardcoding admin







  by @AgentFlow

- *(uncategorized)* Resolve active user from session token instead of hardcoding admin







  by @AgentFlow

- *(uncategorized)* Improve quick-start UX, default env vars, and GitHub sign-ups







  by @AgentFlow

- *(uncategorized)* Relay cancel_task keeps Cancelled state so executor poller detects it







  by @AgentFlow

- *(uncategorized)* Move plan storage to Redis SharedStore, replace FORGE polling with NEXUS notification







  by @AgentFlow

- *(uncategorized)* Remove needless borrow flagged by clippy -D warnings







  by @AgentFlow

- *(uncategorized)* Recycle dead chats on error and prevent stale binaries from silently running







  by @AgentFlow

- *(uncategorized)* Prevent infinite workspace recreation loop when status is unknown







  by @AgentFlow

- *(uncategorized)* Wait for agent to be fully Ready, not just Starting







  by @AgentFlow

- *(uncategorized)* Add confirmation step before starting controller







  by @AgentFlow

- *(uncategorized)* Add start_controller parameter to prevent auto-start during bootstrap







  by @AgentFlow

- *(uncategorized)* Controller reports 'no assignable work' despite tickets in repository







  by @AgentFlow

- *(uncategorized)* Include openflows-harness in dev-sync binary sync







  by @AgentFlow

- *(uncategorized)* Update anyhow to v1.0.104 to fix undefined behavior advisory (RUSTSEC-2024-0436)







  by @AgentFlow

- *(uncategorized)* Track orchestration files and fix clippy errors







  by @AgentFlow

- *(uncategorized)* Agent orchestration pipeline - 6 critical bugs







  by @AgentFlow

- *(uncategorized)* Cycle nexus→forge_pair while workers are busy, not stop







  by @AgentFlow

- *(uncategorized)* Break nexus<->forge_pair infinite loop and fix v2 registry slot derivation







  by @AgentFlow

- *(uncategorized)* Surface FORGE startup errors on first rapid exit and quiet poller log noise







  by @AgentFlow

- *(uncategorized)* Connect Coder workspaces to compose network and fix agent download URL







  by @AgentFlow

- *(uncategorized)* Use ghcr.io/coder/coder image instead of non-existent coder/coder







  by @AgentFlow

- *(uncategorized)* Use --env-file /dev/null for docker-compose to avoid parsing project .env







  by @AgentFlow

- *(uncategorized)* Add Coder bootstrap and docker-compose auto-start to running binary







  by @AgentFlow

- *(uncategorized)* Address PR review — filter duplicate --output-format and apply cargo fmt







  by @AgentFlow

- *(uncategorized)* Sentinel --sandbox flag for Claude backend + inotify fallback







  by @AgentFlow

- *(uncategorized)* Omit issue linking tag from PR prompt when issue number is zero







  by @AgentFlow

- *(uncategorized)* Address PR review feedback — prevent silent identity fallback and comment spam







  by @AgentFlow

- *(uncategorized)* Use dynamic token-based GitHub identity resolution for issue assignment







  by @AgentFlow

- *(uncategorized)* Make crates.io publish tolerant of already-published crates







  by @AgentFlow

- *(uncategorized)* Vessel node now uses resolver registry and searches OPENFLOWS_HOME first







  by @AgentFlow

- *(uncategorized)* Report base registry id in inactive-agent error message







  by @Gitar

- *(uncategorized)* Apply rustfmt formatting to registry.rs







  by @Gitar

- *(uncategorized)* Also check suffix-stripped base id when detecting inactive agents in resolve_github_token







  by @Gitar

- *(uncategorized)* Comprehensive defense against registry/token/path bugs







  by @AgentFlow

- *(uncategorized)* Reject candidates inside orchestration/agent to prevent doubled paths







  by @AgentFlow

- *(uncategorized)* Address PR reviews — backup from OPENFLOWS_HOME, extract helper, add reset recovery hint







  by @AgentFlow

- *(uncategorized)* Proper indentation for install.sh registry backup blocks







  by @AgentFlow

- *(uncategorized)* Preserve user registry.json during install and preserve on reset







  by @AgentFlow

- *(uncategorized)* Prioritize OPENFLOWS_HOME registry and gracefully skip lore on token error







  by @AgentFlow

- *(uncategorized)* Use cargo-zigbuild with glibc 2.17 for gnu targets







  by @AgentFlow

- *(uncategorized)* Remove unsafe musl→gnu fallback and add aarch64 cross-linker env var







  by @AgentFlow

- *(uncategorized)* Address PR review feedback — glibc compat and aarch64-musl fallback







  by @AgentFlow

- *(uncategorized)* Add x86_64-unknown-linux-gnu release target and musl fallback in install script







  by @AgentFlow

- *(uncategorized)* Set executable permissions on .sh files and restore trap in install.sh







  by @AgentFlow

- *(uncategorized)* Address gitar review feedback







  by @AgentFlow

- *(uncategorized)* Track all orchestration files in git and remove duplicate profile







  by @AgentFlow

- *(uncategorized)* Update 'cd AgentFlow' to 'cd openflows' in all docs







  by @AgentFlow

- *(uncategorized)* Update repo URL from AgentFlow to openflows and make install copies orchestration/







  by @AgentFlow

- *(uncategorized)* Make install copies orchestration/ and document cargo install caveat







  by @AgentFlow

- *(uncategorized)* OPENFLOWS_HOME used directly without double-appending /.openflows in load_env()







  by @AgentFlow

- *(uncategorized)* Standardize on OPENFLOWS_HOME, propagate .env parse errors, remove /tmp fallback







  by @AgentFlow

- *(uncategorized)* Load .env from ~/.openflows, install orchestration config, and improve Quick Start







  by @AgentFlow

- *(uncategorized)* Use unscoped npm package name and remove non-existent optionalDeps







  by @AgentFlow

- *(uncategorized)* Use edge docker tag for prereleases and jq for edge tag resolution







  by @AgentFlow

- *(uncategorized)* Exclude dev tags from stable tag lookup in release workflow







  by @AgentFlow

- *(uncategorized)* Address all PR review feedback







  by @AgentFlow

- *(uncategorized)* Strip provider prefix from FIREWORKS_MODEL and OPENAI_MODEL env var fallbacks







  by @AgentFlow

- *(uncategorized)* Drain MCP stderr in background task and use valid fallback model







  by @AgentFlow

- *(uncategorized)* Tighten needs_host_push heuristic and correct agent-system docs







  by @AgentFlow

- *(uncategorized)* Resolve CI failures







  by @AgentFlow

- *(uncategorized)* Rewrite domain step with Layout constraints, richer descriptions







  by @AgentFlow

- *(uncategorized)* Break infinite CI fix loop — nexus guard, annotations enrichment, diagnostic logging







  by @AgentFlow

- *(uncategorized)* Create temp registry.json in nexus e2e test instead of reading from workspace







  by @AgentFlow

- *(uncategorized)* Resolve nexus e2e test by using CARGO_MANIFEST_DIR for workspace-relative paths







  by @AgentFlow

- *(uncategorized)* Resolve CI failures (spelling, format)







  by @AgentFlow

- *(uncategorized)* Clippy warnings, format, and test fixes for CI







  by @AgentFlow

- *(uncategorized)* Accept newer model ID formats, fix fireworks routing, update default model







  by @AgentFlow

- *(uncategorized)* Strip provider prefix from model IDs before passing to CLI backends and API clients







  by @AgentFlow

- *(uncategorized)* Override cli and model_backend from provider when loading existing registry







  by @AgentFlow

- *(uncategorized)* Add back Anthropic support alongside OpenAI and Fireworks







  by @AgentFlow

- *(uncategorized)* MCP startup readiness detection, stream completion events, and worktree nesting guard







  by @AgentFlow

- *(uncategorized)* Disable non-function tool types for SSE custom providers







  by @AgentFlow

- *(uncategorized)* UTF-8 panic in response truncation and Codex v0.133.0 compatibility







  by @AgentFlow

- *(uncategorized)* Tune snif review indexing timeout







  by @Assah Bismark

- *(uncategorized)* Bump snif to v3.2.7 for provider rate limit fix







  by @Assah Bismark

- *(uncategorized)* Use ubuntu-22.04 for gnu builds instead of cargo-zigbuild







  by @AgentFlow

- *(uncategorized)* Address PR review feedback — glibc compat and aarch64-musl fallback







  by @AgentFlow

- *(uncategorized)* Add x86_64-unknown-linux-gnu release target and musl fallback in install script







  by @AgentFlow

- *(uncategorized)* Prefix unused config param with underscore







  by @AgentFlow

- *(uncategorized)* Generate registry.json in setup wizard, fix next steps commands







  by @AgentFlow

- *(uncategorized)* Resolve orchestration files relative to binary in agentflow.rs







  by @AgentFlow

- *(uncategorized)* Resolve orchestration files relative to binary location







  by @AgentFlow

- *(uncategorized)* Point JS wrappers to agentflow binaries







  by @AgentFlow

- *(uncategorized)* Add musl fallback for x86_64 Linux binary download







  by @AgentFlow

- *(uncategorized)* Replace unstable floor_char_boundary with stable char_indices







  by @AgentFlow

- *(uncategorized)* Update Rust to 1.88 for instability crate requirement







  by @AgentFlow

- *(uncategorized)* Update Rust to 1.85 for edition2024 support, fix package name







  by @AgentFlow

- *(uncategorized)* Use rustls instead of native-tls in anthropic-mock







  by @AgentFlow

- *(uncategorized)* Add OpenSSL dev headers to Linux build, fix binary names in install script







  by @AgentFlow

- *(uncategorized)* Add scrolling for GitHub PAT fields when they overflow screen







  by @AgentFlow

- *(uncategorized)* Set input field height to 3 for proper border rendering







  by @AgentFlow

- *(uncategorized)* Implementing plugin module for codex







  by @ndefokou

- *(uncategorized)* Pass CLI backend from PairConfig to ProcessManager







  by @ndefokou

- *(uncategorized)* Use new_with_registry in real_test to pass CLI backend config







  by @ndefokou

- *(uncategorized)* Add debug logging for CLI backend resolution







  by @ndefokou

- *(uncategorized)* Propagate CLI backend from registry to PairConfig







  by @ndefokou

- *(uncategorized)* Respect GITHUB_MCP_CMD in connect_hosted_with_token for test mocking







  by @AgentFlow

- *(uncategorized)* Resolve clippy warnings - implement FromStr trait and remove dead code







  by @AgentFlow

- *(uncategorized)* Suppress unused variable warning in github client test







  by @Christian Yemele

- *(uncategorized)* Resolve CI failures (format, clippy, license)







  by @Christian Yemele

- *(uncategorized)* Close GitHub issues on merge and fix worktree path mismatch







  by @Christian Yemele

- *(uncategorized)* Resolve CI fix rework loop — FORGE re-enters implementation mode instead of fixing CI failures







  by @Christian Yemele

- *(uncategorized)* Set release-type major for semver-checks (0.x breaking changes allowed)







  by @Christian Yemele

- *(uncategorized)* Resolve semver-checks by setting release-type minor and bumping crate versions







  by @Christian Yemele

- *(uncategorized)* Collapse match guard in agent-nexus for clippy







  by @Christian Yemele

- *(uncategorized)* Resolve clippy, fmt, cargo-deny, and semver-checks CI failures







  by @Christian Yemele

- *(uncategorized)* Worktree path resolution and force-with-lease stale info







  by @Christian Yemele

- *(uncategorized)* Resolve merge conflict infinite loop — unrelated histories, force-push, duplicate PR prevention







  by @Christian Yemele

- *(uncategorized)* Resolve FORGE-SENTINEL lifecycle failures preventing PR creation and VESSEL invocation







  by @Christian Yemele

- *(uncategorized)* Simplify env config - proxy mode doesn't require individual API keys







  by @Christian Yemele

- *(uncategorized)* Handle empty STATUS.json race in read_status to prevent sentinel review EOF parse error







  by @Christian Yemele

- *(uncategorized)* Add ticket state machine, remove nested tokio runtime, and fix worktree cleanup







  by @Christian Yemele

- *(uncategorized)* Add debounce, SENTINEL tracking, and output logging to prevent system hangs







  by @Christian Yemele

- *(uncategorized)* Improve NEXUS persona to clarify worker_slots format







  by @Christian Yemele

- *(uncategorized)* STATUS.json parsing and nexus issue discovery







  by @Christian Yemele

- *(uncategorized)* Respect GITHUB_MCP_CMD in connect_hosted_with_token for test mocking







  by @AgentFlow

- *(uncategorized)* Resolve clippy warnings - implement FromStr trait and remove dead code







  by @AgentFlow

- *(uncategorized)* Suppress unused variable warning in github client test







  by @Christian Yemele

- *(uncategorized)* Resolve CI failures (format, clippy, license)







  by @Christian Yemele

- *(uncategorized)* Close GitHub issues on merge and fix worktree path mismatch







  by @Christian Yemele

- *(uncategorized)* Resolve CI fix rework loop — FORGE re-enters implementation mode instead of fixing CI failures







  by @Christian Yemele

- *(uncategorized)* Set release-type major for semver-checks (0.x breaking changes allowed)







  by @Christian Yemele

- *(uncategorized)* Resolve semver-checks by setting release-type minor and bumping crate versions







  by @Christian Yemele

- *(uncategorized)* Collapse match guard in agent-nexus for clippy







  by @Christian Yemele

- *(uncategorized)* Resolve clippy, fmt, cargo-deny, and semver-checks CI failures







  by @Christian Yemele

- *(uncategorized)* Worktree path resolution and force-with-lease stale info







  by @Christian Yemele

- *(uncategorized)* Resolve merge conflict infinite loop — unrelated histories, force-push, duplicate PR prevention







  by @Christian Yemele

- *(uncategorized)* Resolve FORGE-SENTINEL lifecycle failures preventing PR creation and VESSEL invocation







  by @Christian Yemele

- *(uncategorized)* Simplify env config - proxy mode doesn't require individual API keys







  by @Christian Yemele

- *(uncategorized)* Handle empty STATUS.json race in read_status to prevent sentinel review EOF parse error







  by @Christian Yemele

- *(uncategorized)* Add ticket state machine, remove nested tokio runtime, and fix worktree cleanup







  by @Christian Yemele

- *(uncategorized)* Add debounce, SENTINEL tracking, and output logging to prevent system hangs







  by @Christian Yemele

- *(uncategorized)* Improve NEXUS persona to clarify worker_slots format







  by @Christian Yemele

- *(uncategorized)* STATUS.json parsing and nexus issue discovery







  by @Christian Yemele

### Refactor

- *(uncategorized)* Rename orchestration volume to artifacts







  by @AgentFlow

- *(uncategorized)* Apply line wrapping to formatting strings and assertions for improved readability







  by @AgentFlow

- *(uncategorized)* Call current_exe() once and derive parent dirs from single result







  by @AgentFlow

- *(uncategorized)* Reduce embedded files from 80 to 9 critical files







  by @AgentFlow

- *(uncategorized)* Simplify provider support to Codex + OpenAI/Fireworks only







  by @AgentFlow

- *(uncategorized)* Rename all agentflow binaries to openflows







  by @AgentFlow

- *(uncategorized)* Rewrite README to focus on project overview, not implementation details







  by @AgentFlow

### Documentation

- *(a2a)* Document v1 pair-scope trust model honestly







  by @AgentFlow

- *(readme)* Expand npm installation and usage instructions







  by @AgentFlow

- *(uncategorized)* Keep organization-workspace-access in edit-roles example







  by @AgentFlow

- *(uncategorized)* Enhance quick start guide with OAuth user permissions and roles







  by @AgentFlow

- *(uncategorized)* Clarify that bootstrap builds both openflows and openflows-harness







  by @AgentFlow

- *(uncategorized)* Clarify Quick Start order - add tenant before starting controller







  by @AgentFlow

- *(uncategorized)* Remove autonomous mode — CLI mode only for OpenFlows agents







  by @AgentFlow

- *(uncategorized)* Switch default model routing to AI Gateway (Premium license available)







  by @AgentFlow

- *(uncategorized)* Add ephemeral Coder workspace integration plan







  by @AgentFlow

- *(uncategorized)* Add article — OpenFlows and Coder: The Missing Piece







  by @AgentFlow

- *(uncategorized)* Add OpenFlows x Coder integration architecture design doc







  by @AgentFlow

- *(uncategorized)* Update CLI backend docs and add domain config env vars







  by @AgentFlow

- *(uncategorized)* Clarify agent system — add harness system section, concrete examples, and extension path guidance







  by @AgentFlow

- *(uncategorized)* Link contribution_guidelines.md from CONTRIBUTING.md







  by @micheal-ndoh

- *(uncategorized)* Add contribution_guidelines.md for branching, commits and PR standards







  by @micheal-ndoh

- *(uncategorized)* Refactor README - separate installation guide into INSTALL.md







  by @NkwaTambe

- *(uncategorized)* Update Discord link to correct community URL







  by @Christian Yemele

- *(uncategorized)* Improve README with clear project description and autonomous workflow







  by @Christian Yemele

- *(uncategorized)* Refactor CONTRIBUTING.md for better contributor onboarding







  by @Christian Yemele

- *(uncategorized)* Update SharedStore documentation with TUI monitoring interface details and enhance architecture overview







  by @Ngha-Boris

- *(uncategorized)* Update SharedStore documentation with TUI monitoring interface details and enhance architecture overview







  by @Ngha-Boris

- *(uncategorized)* SharedStore documentation with detailed architecture overview and API reference







  by @Ngha-Boris

- *(uncategorized)* Add CLI backend configuration documentation







  by @ndefokou

- *(uncategorized)* Clarify environment configuration for contributors







  by @Christian Yemele

- *(uncategorized)* Rewrite demo.md as contributor walkthrough, link from README and CONTRIBUTING







  by @Christian Yemele

- *(uncategorized)* Clarify STATUS.json location and SENTINEL evaluation workflow







  by @Christian Yemele

- *(uncategorized)* Add comprehensive tutorial, setup checker, and demo recording guide







  by @Christian Yemele

- *(uncategorized)* Add comprehensive demo guide and update README







  by @Christian Yemele

- *(uncategorized)* Add FORGE-SENTINEL architecture design







  by @Christian Yemele

- *(uncategorized)* Remove design.md and update references







  by @Christian Yemele

- *(uncategorized)* Move design.pdf to docs/ and update links







  by @Christian Yemele

- *(uncategorized)* Update installation directory







  by @JOELNATHAN544

- *(uncategorized)* Add contribution steps (design reading, test verification, and issue-based onboarding)







  by @Christian Yemele

- *(uncategorized)* Improve onboarding, setup instructions, and contribution guide







  by @Christian Yemele

- *(uncategorized)* Clarify environment configuration for contributors







  by @Christian Yemele

- *(uncategorized)* Rewrite demo.md as contributor walkthrough, link from README and CONTRIBUTING







  by @Christian Yemele

- *(uncategorized)* Clarify STATUS.json location and SENTINEL evaluation workflow







  by @Christian Yemele

- *(uncategorized)* Add comprehensive tutorial, setup checker, and demo recording guide







  by @Christian Yemele

- *(uncategorized)* Add comprehensive demo guide and update README







  by @Christian Yemele

- *(uncategorized)* Add FORGE-SENTINEL architecture design







  by @Christian Yemele

- *(uncategorized)* Remove design.md and update references







  by @Christian Yemele

- *(uncategorized)* Move design.pdf to docs/ and update links







  by @Christian Yemele

- *(uncategorized)* Update installation directory







  by @JOELNATHAN544

- *(uncategorized)* Add contribution steps (design reading, test verification, and issue-based onboarding)







  by @Christian Yemele

- *(uncategorized)* Improve onboarding, setup instructions, and contribution guide







  by @Christian Yemele

### Styling

- *(uncategorized)* Fix formatting and trailing whitespace







  by @AgentFlow

- *(uncategorized)* Fix formatting for CI







  by @AgentFlow

- *(uncategorized)* Simplify welcome page to clean minimal design







  by @AgentFlow

- *(uncategorized)* Redesign OpenFlows welcome page with clean boxed logo







  by @AgentFlow

- *(uncategorized)* Redesign TUI with cyberpunk terminal aesthetic







  by @AgentFlow

- *(uncategorized)* Apply cargo fmt formatting







  by @AgentFlow

- *(uncategorized)* Apply cargo fmt formatting







  by @AgentFlow

### Testing

- *(uncategorized)* Add codex E2E test variants for nexus







  by @AgentFlow

### Miscellaneous Tasks

- *(uncategorized)* Sync Cargo.lock to 1.2.1 for openflows and openflows-harness







  by @AgentFlow

- *(uncategorized)* Bump openflows and openflows-harness to 1.2.1







  by @AgentFlow

- *(uncategorized)* Trigger release-plz scan for dummy PR







  by @AgentFlow

- *(uncategorized)* Allowlist gitar spelling in typos config







  by @AgentFlow

- *(uncategorized)* Apply rustfmt to openflows-harness crate (PR #141)







  by @AgentFlow

- *(uncategorized)* Disable automatic release on main push, only on tags or manual dispatch







  by @AgentFlow

- *(uncategorized)* Remove snif code review workflow and config







  by @AgentFlow

- *(uncategorized)* Bump version to 1.0.16







  by @AgentFlow

- *(uncategorized)* Add project config module, domain provisioning, and various refinements







  by @AgentFlow

- *(uncategorized)* Update cargo lock and npm version







  by @AgentFlow

- *(uncategorized)* Align plugin structure with orchestration/ centralized architecture







  by @ndefokou

- *(uncategorized)* Add LICENSE file







  by @Christian Yemele

- *(uncategorized)* Project cleanup, rename binary to agentflow, and update docs







  by @AgentFlow

- *(uncategorized)* Align plugin structure with orchestration/ centralized architecture







  by @ndefokou

- *(uncategorized)* Add LICENSE file







  by @Christian Yemele

### Continuous Integration

- *(uncategorized)* Fix action versions using tags instead of commit SHAs







  by @AgentFlow

- *(uncategorized)* Add PR linter job validating titles and template sections







  by @AgentFlow

- *(uncategorized)* Retry CI checks







  by @AgentFlow

### Reverted Commits

- *(uncategorized)* Restore all 80 embedded orchestration files







  by @AgentFlow

- *(uncategorized)* Restore scoped @the-agenticflow/openflows package name







  by @AgentFlow

- *(uncategorized)* Keep registry.json domain defaults unchanged







  by @AgentFlow

### Hardening

- *(uncategorized)* Shared dir in worktree, MCP timeouts, Claude provisioning, RUST_LOG support







  by @AgentFlow

### A2a-protocol

- *(uncategorized)* Add verify task schema and Redis key helpers







  by @AgentFlow

### Merge

- *(uncategorized)* Resolve conflicts with main — keep ubuntu-22.04 gnu builds, remove zigbuild







  by @AgentFlow

- *(uncategorized)* Resolve conflicts with main, preserving all VESSEL agent and main features







  by @Christian Yemele

- *(uncategorized)* Resolve conflicts with main, preserving all VESSEL agent and main features







  by @Christian Yemele

### Release

- *(uncategorized)* V1.1.8 — fix worker workspaces booting without openflows-harness







  by @AgentFlow

- *(uncategorized)* Bump version to 1.1.6







  by @AgentFlow

### Remove

- *(uncategorized)* Delete auto-sync GitHub Action — local VESSEL sync is sufficient







  by @Christian Yemele

- *(uncategorized)* Delete auto-sync GitHub Action — local VESSEL sync is sufficient







  by @Christian Yemele





