# Changelog

All notable changes to this project will be documented in this file.
<!-- markdownlint-disable line-length no-bare-urls ul-style emphasis-style -->

## [1.9.1] - 2026-11-27

### Features

- Setup: Add console.sh to start the Console against any running Manager (e9ce419) (@arielpetit)

### Bug fixes

- Build: Build the Manager image with Rust 1.95.0 (036f02d) (@arielpetit)
- Deps: Update kube to 5.2.0 and url to 2.5.9 for RUSTSEC-2026-0600 and -0592 (a2039bb) (@arielpetit)
- Setup: Make console.sh recover from interrupted runs and stay in its lane (602553a) (@arielpetit)
- Setup: Keep a working Console on failure and match the sign-in host in console.sh (24ff787) (@arielpetit)


### Contributors

- @arielpetit

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.9.0...openflows-1.9.1

## [1.9.0] - 2026-11-20

### Features

- Hosted: Add a merge broker purpose for the controller's Vessel (2c1fd85) (@onelrian)

### Bug fixes

- Coder Client: Decode the connecting agent status and fall back on unseen ones (5cfb546) (@onelrian)
- Hosted: Keep runtime rotation out of the broker deadline (96dfd50) (@onelrian)
- Hosted: Heartbeat while the Vessel credential is retried (5097205) (@onelrian)
- Hosted: Let the controller's Vessel token read CI (92d1b61) (@onelrian)
- Hosted: Bound the Vessel token exchange and wait for it before routing (4bb8a97) (@onelrian)
- Hosted: Exchange the Vessel GitHub token before the first flow pass (45d5ed7) (@onelrian)
- Hosted: Keep the in-process Vessel's GitHub token current from the broker (94e8b60) (@onelrian)
- K8s templates: Replace only an empty workspace directory and fail on one with files (ee999fd) (@onelrian)
- K8s templates: Move a root-owned workspace directory aside on a kept claim (1d6a3eb) (@onelrian)
- K8s templates: Create the workspace directory as the workspace user before startup (dfe9290) (@onelrian)
- Nexus template: Create the relay Service only once a tenant is named (19e77a6) (@onelrian)

### Documentation

- Hosted: List workflows write on the Forge git profile (165b1d7) (@onelrian)
- Hosted: Record the CI reads the controller's Vessel node now needs (dbcce0d) (@onelrian)
- Runtime: Warn that recreating a tenant is destructive and correct the chat error text (e14a0bb) (@onelrian)
- Runtime: Add the Kubernetes first-run checklist and the workspace directory init container (f47536a) (@onelrian)
- Show OpenFlows logo and Kubernetes backend (9c52dfb) (@Christiantyemele)
- Polish README header (207a604) (@Christiantyemele)
- Refresh project README (ea54149) (@Christiantyemele)

### Code refactoring

- Coder Client: Nest the AgentStatus module under types (bdae32e) (@onelrian)
- Coder Client: Move AgentStatus into its own module to stay under the line limit (bb4a213) (@onelrian)

### Tests

- Coder Client: Make the readiness check depend on the agent status alone (9bed6ef) (@onelrian)

### Continuous integration

- E2E: Survive a mirror sync race when building the fixture image (d26ba68) (@onelrian)

### Chores

- Hosted: Tighten the CI-read profile test and broker doc wording (873f4bc) (@onelrian)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.8.3...openflows-1.9.0

## [1.8.3] - 2026-11-17

### Features

- Compose: Run the Manager as a Compose service next to the Console (bb3b075) (@arielpetit)
- Harness: Pin the artifact bundle to the release the harness was built with (b01b78d) (@onelrian)
- Runtime: Integrate workspace-runtime release selection and template cleanup (d269267) (@NkwaTambe)
- Runtime: Run the four worker roles as Kubernetes pods (c2e59a1) (@onelrian)
- Runtime: Run the Nexus workspace as a Kubernetes pod (649b289) (@onelrian)
- Runtime: Extract shared role bootstrap and publish a pinned workspace image (04cda5b) (@ndefokou)
- Runtime: Enforce who may bind the secret writer role (6c81c9a) (@lele-maxwell)
- Runtime: Separate the secret delivery identity from namespace setup (61c2d3a) (@lele-maxwell)
- Runtime: Isolate tenants on kubernetes and verify it at startup (0a9dd1e) (@lele-maxwell)
- Setup: Add up.sh to start the control plane and Console in one command (b58a9a3) (@arielpetit)
- Templates: Mount tenant artifacts volume only in local mode (0266021) (@onelrian)

### Bug fixes

- Build: Watch only the variant tree, not missing per-role sources (ee0b253) (@onelrian)
- Build: Rerun the template packer when a role source appears (450978b) (@onelrian)
- Manager: Require runtime manifest sha in hosted mode and enforce repo allowlist (fc28469) (@NkwaTambe)
- Manager: Treat an empty Coder OAuth client ID as not configured (6d68725) (@lele-maxwell)
- Runtime: Address Greptile review findings and restore E2E suites (71e26fe) (@NkwaTambe)
- Runtime: Replace the workspace pod before starting the next one (f6b1cf7) (@onelrian)
- Runtime: Keep Lore's author identity on the pod (6031dd4) (@onelrian)
- Runtime: Keep the Docker ownership repair and unblock the first pod build (983dc8c) (@onelrian)
- Runtime: Address review of the shared role bootstrap (da6579e) (@ndefokou)
- Runtime: Check identity separation inside each namespace (9e73a53) (@lele-maxwell)
- Runtime: Tighten tenant isolation after second review (7a03630) (@lele-maxwell)
- Runtime: Address review of tenant isolation (155ee91) (@lele-maxwell)
- Setup: Create the Manager's keys before it has to start (487f680) (@lele-maxwell)
- Setup: Remove duplicate Manager keys from the connected-local compose file (232fd9c) (@lele-maxwell)
- Setup: Clear a standalone Console and wait for the Console in up.sh (2d918b8) (@arielpetit)
- Setup: Resolve secret paths like Compose and check only the files it mounts (c56ba26) (@arielpetit)
- Setup: Make up.sh stop with a clear message when the operator secrets are missing (ba9ff95) (@arielpetit)
- Templates: Create the Nexus artifacts directory before chowning it (4fbfd7b) (@onelrian)
- Ignore RUSTSEC-2026-0543 (chrono unmaintained) in cargo-deny (0033da2) (@NkwaTambe)
- Resolve runtime release from manifest and add tenant alias for local nexus (527f8a6) (@NkwaTambe)

### Documentation

- Runtime: Describe how startup works without privilege in a pod (ba7caed) (@onelrian)
- Runtime: Record the hosted upgrade path and the clone contract (9a27075) (@onelrian)
- Runtime: Scope the artifact pin to the bundle and name harness_version (8a394f1) (@onelrian)
- Runtime: Correct seed failure handling and separate integrity from release pinning (546a755) (@onelrian)
- Runtime: Inventory what the shared artifacts volume still delivers (cc2c5ee) (@onelrian)
- Remove orphan containers when cleaning up a release test (0d5a7fb) (@lele-maxwell)
- Match the setup order and add a release install test (efed3d2) (@lele-maxwell)

### Code refactoring

- Manager: Move template archives and workspace parameters into modules (695fbd5) (@onelrian)
- Tests: Split repository runtime tests out to stay under the line cap (d1ff4e8) (@ndefokou)

### Tests

- E2E: Verify published workspace-runtime v0.3.1 without dev mounts (2559ff2) (@NkwaTambe)
- Runtime: Drop the unused PermissionsExt import (c48199f) (@onelrian)

### Continuous integration

- Ignore changelog commit hashes in spelling check (0cb3ddd) (@Christiantyemele)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.8.2...openflows-1.8.3

## [1.8.2] - 2026-11-12

### Features

- API: Document Manager and Agent Nexus HTTP endpoints with utoipa (417360f) (@NkwaTambe)
- Auth: Complete connected-local GitHub user token flow (54eaa70) (@Christiantyemele)
- Auth: Begin connected local GitHub user tokens (e8941bc) (@Christiantyemele)
- Compose: Run the Console from its GHCR image (ceba05d) (@arielpetit)
- Compose: Install OpenFlows from the published images (ac616a5) (@lele-maxwell)
- Config: Add connected local rollout flag (7a350c4) (@Christiantyemele)
- Harness: Install the workspace artifact bundle from the Manager (7dd6f50) (@onelrian)
- Local: Add connected GitHub console journey (735f299) (@Christiantyemele)
- Lore: Make LORE a controller-driven docs agent (b3cb0d4) (@ndefokou)
- Manager: Serve the workspace artifact bundle by content digest (7b5249c) (@onelrian)
- Manager: Select and validate template releases by backend (6203028) (@NkwaTambe)
- Manager: Deliver workspace runtime credentials separately from the secret store (a39bbf1) (@onelrian)
- Manager: Opt-in request metrics, release checklist and rollback (d26f97f) (@Jagoum)
- Manager: Report what a tenant's agents are doing (213e619) (@arielpetit)
- Manager: Preview an invitation before accepting it (dd43546) (@arielpetit)
- Manager: List an organization's operation history (33db8a2) (@arielpetit)
- Manager: Report Nexus runtime health on tenant status (a673e5e) (@arielpetit)
- Manager: Report provisioning state on organization detail (702aaa4) (@arielpetit)
- Manager: Show who a pending invitation is for (59e066f) (@arielpetit)
- Manager: Add read endpoints for the Console (cb8581c) (@arielpetit)
- Setup: Show the repo and App mode and check your own App (03c780c) (@lele-maxwell)
- Setup: Run the shared App without compiling (845e981) (@lele-maxwell)
- Setup: Install from release binaries and run compose on any OS (d64fbcc) (@lele-maxwell)

### Bug fixes

- API: Align OpenAPI models with serialized responses and request schemas (018bb96) (@NkwaTambe)
- API: Add request/response bodies, headers, and hook auth to OpenAPI (01aaf15) (@NkwaTambe)
- API: Address remaining Greptile review findings (d4ffc37) (@NkwaTambe)
- API: Address OpenAPI review findings (261d928) (@NkwaTambe)
- Auth: Address connected-local review findings (b1ec7d6) (@Christiantyemele)
- Compose: Address review on host support, Compose version and notes (c472b3c) (@lele-maxwell)
- Compose: Drop the template comparison; templates ship inside the programs (fbb3bd4) (@lele-maxwell)
- Docker: Follow registry pagination when promoting manager floating tags (5a58d2e) (@onelrian)
- Docker: Promote manager floating tags in a serialized job from the registry state (d92efb3) (@onelrian)
- Docker: Move floating manager tags after the build and keep per-tag builds (885f8f5) (@onelrian)
- Docker: Move floating manager tags only for the newest release (8e8bc35) (@onelrian)
- GitHub: Paginate list_review_comments across all pages (e5e3b5a) (@NkwaTambe)
- Harness: Replace the bundle on install and fetch it after the harness exists (9157943) (@onelrian)
- Harness: Keep the runtime token out of arguments and skip retrying a 4xx (93a6381) (@onelrian)
- Local: Reconcile shared App setup with develop onboarding (aa65278) (@Christiantyemele)
- Local: Parse worker clone URLs for broker requests (0542db1) (@Christiantyemele)
- Local: Broker GitHub App credentials to local runtimes (1728c5a) (@Christiantyemele)
- Local: Gate account controls and persist auth mode (574a4ac) (@Christiantyemele)
- Local: Protect device sign-in and document routes (19af1f2) (@Christiantyemele)
- Local: Address connected-local review findings (8a77bbf) (@Christiantyemele)
- Local: Address connected-local review findings (cbd0ab8) (@Christiantyemele)
- Local: Establish Manager session after device authorization (9afaec3) (@Christiantyemele)
- Lore: Only an operator decision grants the protected docs approval (1b6ad5e) (@ndefokou)
- Lore: Commit operator docs decisions atomically; hold heads that moved during CI (c54e36f) (@ndefokou)
- Lore: Close the docs gate review findings (fdcc085) (@ndefokou)
- Lore: Resolve the docs repository from the tenant store only (2c25239) (@ndefokou)
- Manager: Correct unparsable typo in coder version doc (67955e2) (@Christiantyemele)
- Manager: Claim release pin before coder create and strengthen workspace validation (b9a8b3f) (@NkwaTambe)
- Manager: Serialize tenant and workspace release pinning (c346837) (@NkwaTambe)
- Manager: Isolate coder template versions and harden legacy backend pinning (b8fc35f) (@NkwaTambe)
- Manager: Retire workspace secrets when the organization has no Coder id (58d185c) (@onelrian)
- Manager: Address 505 review on delivery mounts, cleanup and line limits (5155f2f) (@onelrian)
- Manager: Let a removed member rejoin through a fresh invitation (765a03b) (@arielpetit)
- Scripts: Run the dev-sync Docker build as the host user (b4d253d) (@Jagoum)
- Setup: Request every permission the agents use on a new App (a9b48fd) (@lele-maxwell)
- Setup: Give new Apps the Issues and Checks permissions (685edd5) (@lele-maxwell)
- Setup: Make prod.sh use the Coder port from .env (97473e1) (@lele-maxwell)
- Setup: Create .dev-binaries before Docker can (4f178b6) (@lele-maxwell)
- Setup: Stop .env.example from breaking prod.sh (0537163) (@lele-maxwell)
- Setup: Keep the setup log private (4573d5b) (@lele-maxwell)
- Setup: Remove the local Manager after switching to your own App (0c83d93) (@lele-maxwell)
- Setup: Repair programs that lost their permissions (2695825) (@lele-maxwell)
- Setup: Treat an empty image repo in the shell as Compose does (f7ec091) (@lele-maxwell)
- Setup: Read the image repo from .env and restore deleted programs (ac35dbe) (@lele-maxwell)
- Setup: Keep native programs and upgrades consistent (46aa2c2) (@lele-maxwell)
- Setup: Repair damaged programs in binary installs (4563a67) (@lele-maxwell)
- Setup: Keep starting workspaces pending until restarted (55c0ccd) (@lele-maxwell)
- Setup: Honor custom compose path separators (bbe6e4b) (@lele-maxwell)
- Setup: Verify workspace upgrades and keep compose overrides (3e26906) (@lele-maxwell)
- Setup: Keep pending upgrades when Coder does not answer (752657a) (@lele-maxwell)
- Setup: Finish upgrades for every workspace (371a24e) (@lele-maxwell)
- Setup: Keep pending restarts across runs; support Bash 3.2 (f0b6f40) (@lele-maxwell)
- Setup: Push the image's templates again after a source build (cc5d823) (@lele-maxwell)
- Setup: Don't fail when workspace updates are declined (f51b7de) (@lele-maxwell)
- Setup: Update workspaces to the new templates; keep release-note text (bdc1ad5) (@lele-maxwell)
- Setup: Keep Compose overrides for plain source installs (026d502) (@lele-maxwell)
- Setup: Keep default tenant names distinct when normalising the repo (d8d659d) (@Jagoum)
- Templates: Mount dev binaries in the LORE workspace (686765d) (@ndefokou)
- Templates: Map host.docker.internal on Linux and document local setup (1bbc57f) (@arielpetit)
- Tenant: Keep tenant workspace names within Coder's 32-character limit (af8b003) (@Jagoum)
- Vessel: Wait for review when GitHub refuses a merge (6e2cc23) (@Jagoum)
- Vessel: Keep rework evidence and require exact head approvals (f7d9737) (@Jagoum)
- Vessel: Keep valid approvals after rework addresses change requests (6fc9858) (@Jagoum)
- Pass connected local template variables to Manager (217cb8b) (@Christiantyemele)
- Use local workspace image for local tenants (50a7c93) (@Christiantyemele)
- Accept newer Coder and connect local provisioner (59787ec) (@Christiantyemele)
- Keep local device code visible while polling (d177be3) (@Christiantyemele)
- Recover local GitHub installation and account switching (ff6b29c) (@Christiantyemele)
- Run credential helper test with source available (ee75611) (@Christiantyemele)
- Address second review of the Console API changes (31d86da) (@arielpetit)
- Address review of the Console API changes (367c6e6) (@arielpetit)
- Let operators make mounted secret files readable by the workspace user (612e0a7) (@arielpetit)

### Documentation

- API: Add OpenAPI reference verification screenshots (50ee465) (@NkwaTambe)
- Compose: Tell compose users to switch to their own GitHub App (744c66c) (@lele-maxwell)
- Setup: Say why the shared App needs compiling with compose (c8d00e6) (@lele-maxwell)
- Clarify workspace bootstrap ordering (1d7922d) (@Christiantyemele)
- Update the release flow and shared App steps (cdc7fad) (@lele-maxwell)
- Document binary installs and the distribution contract (6177e7d) (@lele-maxwell)
- Summarize connected-local live test (f376b8e) (@Christiantyemele)
- Simplify Coder Premium feature table (8031435) (@Christiantyemele)
- Explain Coder Premium requirements for organization provisioning (6935657) (@Christiantyemele)
- Explain GitHub auth mode before setup (1e1c395) (@Christiantyemele)
- Explain local Manager Coder token setup (1003d36) (@Christiantyemele)
- Document the compose install and distribution modes (eaa9850) (@lele-maxwell)
- Keep rollout flag out of implementation plan (5f665d5) (@Christiantyemele)
- Keep connected local plan out of implementation index (f6aa460) (@Christiantyemele)

### Code refactoring

- Lore: Split the LORE driver and docs lifecycle into modules (f8d44aa) (@ndefokou)
- Manager: Move Coder version compare into versions module (09e9d2e) (@Christiantyemele)
- Manager: Keep grandfathered files within PR line limits (34753a4) (@NkwaTambe)
- Manager: Split tenant secret delivery out of tenant.rs (25c46ec) (@onelrian)
- Manager: Name the provisioning row type (clippy::type_complexity) (890d3be) (@arielpetit)
- Setup: Move the release install code to its own file (d0f9438) (@lele-maxwell)
- Move LORE docs code into focused modules under the line caps (2da0e30) (@ndefokou)

### Tests

- Auth: Cover connected-local GitHub renewal and live flow (b52d110) (@Christiantyemele)
- Auth: Cover connected-local GitHub user token flow (ac63ad4) (@Christiantyemele)
- Config: Isolate connected local default tests (6347bb6) (@Christiantyemele)
- E2E: Approve docs PR as an independent reviewer (a033cae) (@Christiantyemele)
- E2E: Install one candidate release in source, compose and binary modes (31a3560) (@Jagoum)
- E2E: Cover LORE docs and the VESSEL docs PR merge in the connected system (6e09aaa) (@ndefokou)
- E2E: Check both directions alike and record state at every step (9763329) (@Jagoum)
- E2E: Upgrade, restart and roll back a running stack (218b93d) (@Jagoum)
- E2E: Read brokered token in HTTP fixture (29bb521) (@Christiantyemele)
- E2E: Approve the reworked head with a second reviewer (cba2695) (@Jagoum)
- E2E: Verify valid and forged pair tokens on registered pair (9f52030) (@NkwaTambe)
- E2E: Strengthen system e2e with rework loops and human reviews (b1bd591) (@NkwaTambe)
- Manager: Run backend release selection and pinning tests in postgres runner (a3ec79c) (@NkwaTambe)
- Setup: Use runner available grep in onboarding check (87fb926) (@Christiantyemele)
- Vessel: Run two post-driving tests on an 8 MiB stack (13ebbbd) (@ndefokou)
- Vessel: Update inline comment mocks for paginated query (9ea3ce5) (@NkwaTambe)
- Run the two poll-driving tests on an 8 MiB stack (748b7cd) (@ndefokou)
- Cover local GitHub isolation and credential denial (2f3896a) (@Christiantyemele)
- Use a correctly spelled repo name in tenant name tests (86cd3fc) (@Jagoum)

### Build

- Docker: Publish release tags for the manager image like release-plz (cad3ea1) (@onelrian)

### Continuous integration

- Line limits: Skip bounded-growth gate on develop-to-main release PRs (02acd4f) (@Christiantyemele)
- Require shell-tests in the merge gate (fd76a7b) (@Jagoum)

### Chores

- Routes: Rustfmt after merge (f098177) (@NkwaTambe)
- Order route modules after merging the Console endpoints (916af87) (@arielpetit)
- Sync connected-local branch with develop (e8a0279) (@Christiantyemele)
- Sync develop into connected-local flow (208bf94) (@Christiantyemele)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.8.1...openflows-1.8.2

## [1.8.1] - 2026-10-29

### Bug fixes

- Docker: Ship the templates, document /ready and the missing docker cli (ca1a8c1) (@onelrian)
- Docker: Use supported Rust toolchain and permit release rebuild override (c0e3c67) (@Christiantyemele)

### Documentation

- Manager: List every subcommand in the image section (668e4c3) (@onelrian)

### Build

- Docker: Publish the openflows-manager image (dadd7f7) (@onelrian)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.8.0...openflows-1.8.1

## [1.8.0] - 2026-10-28

### Features

- Hooks: Add worker hooks publishing and related tests (ad90b9b) (@Christiantyemele)
- Manager: Add hosted tenant runtimes and operator setup (2ce1f29) (@Christiantyemele)
- Manager: Add hosted template release sources (e869620) (@Christiantyemele)
- Manager: Add Coder template release provisioning (68b7532) (@Christiantyemele)
- Manager: Add runtime credential broker (3444aa4) (@Christiantyemele)
- Manager: Add GitHub App connection lifecycle (80aa958) (@Christiantyemele)
- Manager: Complete WP-02 human auth and org authorization (47cabb6) (@Christiantyemele)
- Manager: Add organization policy, membership, and invitation APIs (1d8c308) (@Christiantyemele)
- Manager: Add browser and CLI device authentication (a1e4c6a) (@Christiantyemele)
- Manager: Add WP-02 auth persistence and crypto primitives (896bc75) (@Christiantyemele)
- Runtime: Enforce role-scoped GitHub delivery (3aa76cc) (@Christiantyemele)
- Complete hosted runtime management flow (6346a0d) (@Christiantyemele)

### Bug fixes

- Deployment: Complete hook startup configuration (bb6d85e) (@Christiantyemele)
- E2E: Drain recovered reviews and prove pending CI holds (19596f8) (@NkwaTambe)
- Manager: Coordinate release cache writers and restore default binary (5e0940f) (@Christiantyemele)
- Manager: Address hosted template release review (9f85737) (@Christiantyemele)
- Manager: Include template release builder (c5ff941) (@Christiantyemele)
- Manager: Align promotion and readiness locks (2b87b47) (@Christiantyemele)
- Manager: Close remaining release review gaps (9a461ae) (@Christiantyemele)
- Manager: Address provisioning review findings (94ab253) (@Christiantyemele)
- Manager: Preserve shared tokens on cached request failures (ae71ae5) (@Christiantyemele)
- Manager: Address runtime credential broker review (a5e379b) (@Christiantyemele)
- Manager: Retain temporary failures for automatic retry (5dfa6b1) (@Christiantyemele)
- Manager: Fence reconnects and bound connection job retries (157c270) (@Christiantyemele)
- Manager: Address GitHub lifecycle review findings (d8c154d) (@Christiantyemele)
- Manager: Address authentication and organization review findings (b572aaf) (@Christiantyemele)
- Manager: Satisfy spelling checks (58b0794) (@Christiantyemele)
- Nexus: Retain rework delivery ids (5cb466a) (@Christiantyemele)
- Nexus: Align managed worker branch resolution (08a32fb) (@Christiantyemele)
- Runtime: Fence tenant cleanup and rework delivery (0bbeda6) (@Christiantyemele)
- Runtime: Address hosted runtime review findings (2f7890b) (@Christiantyemele)
- Runtime: Filter template promotions by supported release (bf8ea30) (@Christiantyemele)
- Runtime: Address remaining review findings (2035d4b) (@Christiantyemele)
- Runtime: Close remaining review threads (c57592e) (@Christiantyemele)
- Runtime: Close remaining hosted lifecycle races (a2ef700) (@Christiantyemele)
- Runtime: Address hosted lifecycle review findings (ad19a5c) (@Christiantyemele)
- Runtime: Address hosted lifecycle review findings (part 2) (c709ceb) (@Christiantyemele)
- Runtime: Forward managed worker assignment (8602a99) (@Christiantyemele)
- Runtime: Address hosted lifecycle review findings (876cf67) (@Christiantyemele)
- Security: Resolve cargo-deny advisories on develop (6940fcd) (@Christiantyemele)
- Address PR creation review feedback (04d5899) (@Christiantyemele)

### Documentation

- Centralized deployment: Add implementation plan and compatibility baseline (fd8608c) (@Christiantyemele)
- E2E: Remove implementation history from test architecture (8061ee4) (@NkwaTambe)
- E2E: Shorten introductory comments (53a61df) (@NkwaTambe)
- GitHub: Require only essential fields in bug report form (8aea868) (@lele-maxwell)
- GitHub: Add issue templates and security policy (9449a96) (@lele-maxwell)
- Define the complete end-to-end testing architecture (a6c9c6e) (@NkwaTambe)

### Tests

- Coder: Cover refreshable hosted gh credentials (3c5c1e6) (@Christiantyemele)
- E2E: Connect the production issue-to-merge workflow (a7d5992) (@NkwaTambe)
- E2E: Preserve failure evidence and verify applied settings and cleanup (3870da7) (@NkwaTambe)
- Lifecycle: Isolate the human approval merge gate (29209b6) (@NkwaTambe)
- Refactor test descriptions and organization (5072d87) (@Christiantyemele)
- Fix fixture readiness, diagnostics, and tool matching (38cf645) (@NkwaTambe)
- Add deterministic system E2E service fixtures (7e62675) (@NkwaTambe)
- Preserve verification log before workspace deletion (0469967) (@NkwaTambe)
- Verify checkout isolation and stale review guards independently (d0c80f5) (@NkwaTambe)

### Continuous integration

- Verify candidate code through real paired workers (77a627f) (@NkwaTambe)
- Gate merges on real container integration tests (02941e7) (@NkwaTambe)

### Chores

- Manager: Remove review handoff file (bc8b7d6) (@Christiantyemele)
- Manager: Format connection flow changes (c1a10dd) (@Christiantyemele)
- Sync main into develop for release promotion (5a0d436) (@Christiantyemele)
- Remove unrelated Zep integration (169fdb9) (@Christiantyemele)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.7.1...openflows-1.8.0

## [1.7.1] - 2026-10-09

### Features

- Manager: Initialize structured tracing (0eccb47) (@Christiantyemele)
- Manager: Add WP-01 manager foundations (1bfbf74) (@Christiantyemele)

### Bug fixes

- Controller: Handle 410 gone and scan pr_opened tickets during rework (f8e63de) (@NkwaTambe)
- Manager: Address remaining review comments (7bf6fd5) (@Christiantyemele)
- Manager: Address WP-01 persistence and test review (67c6a95) (@Christiantyemele)
- Vessel: Resolve active forge slot id instead of hardcoded role name (e5bee41) (@NkwaTambe)
- Vessel: Propagate fresh head sha in reviews outcome and guard lifecycle transitions (3d6f5c0) (@NkwaTambe)
- Vessel: Verify authorized head approval in rework check (d1b5c77) (@NkwaTambe)
- Vessel: Route conflicting unapproved PRs to conflict rework after CI success (6aabe53) (@NkwaTambe)
- Vessel: Inspect unapproved PRs for review rework before gating on merge_ready (568301a) (@NkwaTambe)
- Vessel, nexus: Address Greptile review comments on rework loops (eb25a99) (@NkwaTambe)
- Vessel, nexus: Autonomous PR review rework reset and GitHub approval detection (c54aef6) (@NkwaTambe)

### Tests

- Vessel: Guard collect_rework on empty repo and allocate dedicated stack for mockito test (7fc3a1e) (@NkwaTambe)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.7.0...openflows-1.7.1

## [1.7.0] - 2026-10-08

### Features

- Setup: Ask for the fleet size during setup (51b9240) (@arielpetit)
- Setup: Add one-command setup script and slim the quick start (d3766a1) (@arielpetit)

### Bug fixes

- GitHub: Include startup_failure in failure detail paths and diagnostics (dff2e00) (@NkwaTambe)
- GitHub: Scope ghost check suite filter to known non-ci apps (639eadd) (@NkwaTambe)
- GitHub: Ignore ghost check suites with zero runs in ci status (bb28bf2) (@NkwaTambe)
- GitHub: Do not let non-verdict reviews supersede approvals or change requests (b317b00) (@NkwaTambe)
- Merge: Reconcile GitHub approval into lifecycle so approved PRs merge (8265370) (@NkwaTambe)
- Scripts: Restore the installer hint in prod.sh (7c5ff3f) (@arielpetit)
- Scripts: Use the musl image's linker in dev-sync Docker build (bee4b82) (@arielpetit)
- Setup: Show progress while installing the coder CLI (612975b) (@arielpetit)
- Setup: Check GitHub sign-in is offered and refuse to reuse a tenant for another repo (caffffe) (@arielpetit)
- Setup: Keep the workspace token alive and make re-runs safe (64f022b) (@arielpetit)
- Setup: Address review feedback on the setup script (0412576) (@arielpetit)
- Vessel: Retain workspace references when deletion fails (568eb96) (@NkwaTambe)
- Vessel: Move test_decision guard before pr_human for side-effect-free rejection (aa8cb64) (@NkwaTambe)
- Vessel: Satisfy pr_decision during GitHub approval reconciliation (07cf436) (@NkwaTambe)
- Vessel: Only reconcile authorized current-head GitHub approvals (d3e320a) (@NkwaTambe)
- Vessel, docs: Require sentinel test approval before reconciliation and set checks to read-only (82058d4) (@NkwaTambe)
- Vessel, nexus: Guard workspace lifecycle and worker release (96aa6e8) (@NkwaTambe)
- Vessel, nexus: Recycle forge and sentinel pairs to idle on merge and terminal state (42a5f2d) (@NkwaTambe)
- Vessel, coder client: Trigger delete build and isolate worker slot workspace (2b6b3a8) (@NkwaTambe)
- Vessel, coder client, nexus: Confirm workspace deletion and retry failed cleanups (31fe1e0) (@NkwaTambe)
- Vessel, nexus, config: Safe concurrent cleanup, preserve retryable tickets, archive chats (97ca071) (@NkwaTambe)

### Documentation

- Quickstart: Add Checks and Issues to GitHub App permissions (44ba425) (@NkwaTambe)

### Chores

- Agent Sentinel: Drop unused github and regex dependencies (75e97e9) (@NkwaTambe)
- Apply cargo fmt formatting (14dc367) (@NkwaTambe)
- Fix formatting in agent-vessel (a2bb000) (@NkwaTambe)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.6.0...openflows-1.7.0

## [1.6.0] - 2026-10-06

### Features

- Docs: Add comprehensive state machine whitepaper detailing lifecycle and roles (2722271) (@Christiantyemele)
- Vessel: Implement rework provisioning for tickets without live FORGE chat (91c1562) (@Christian Yemele)
- Add NeedsReview state to PR monitor and enhance review handling in VesselNode (a043f68) (@Christian Yemele)
- Introduce rework directive handling and review type namespaces for SENTINEL (b3f3234) (@Christian Yemele)
- Implement /ci_fix directive handling for CI failures (8db718a) (@Christian Yemele)
- Implement PR-state lifecycle monitor and address review directive (a644224) (@Christian Yemele)

### Bug fixes

- A2A: Authenticate verification pairs (15b9710) (@Christiantyemele)
- Coder: Provision forge verification toolchain and PATH (41a736c) (@Christiantyemele)
- Coder: Use immutable pair token parameter (0ae4f07) (@Christiantyemele)
- Coder: Allow ephemeral a2a pair token parameter (f16bfee) (@Christiantyemele)
- Coder Client: Surface real stderr on coder templates push failure (e8e45c0) (@ndefokou)
- Controller: Bound redis and coder provisioning waits (aed0b7f) (@Christiantyemele)
- Forge: Read root gh token across EOF without failing (ef4187e) (@Christiantyemele)
- Forge: Keep gh best-effort and pass token via stdin (917f41a) (@Christiantyemele)
- Forge: Hard-fail GitHub credential inheritance in workspace startup (8320adf) (@Christiantyemele)
- Forge: Make branch-mismatch marker write non-fatal in startup (95f9669) (@Christian Yemele)
- Forge: Flag branch mismatch when rework cannot reach the PR branch (e4ec3f9) (@Christian Yemele)
- Forge: Make branch-switch failure non-fatal in workspace startup (921b912) (@Christian Yemele)
- Forge: Accept valid PR branches and stop fabricating missing PR branch (1cbd9f7) (@Christian Yemele)
- Forge: Sanitize branch and avoid detached HEAD in workspace startup (66fcb27) (@Christian Yemele)
- GitHub: Deserialize reviewer user object into login for reviews and comments (2870929) (@Christian Yemele)
- Hygiene: Removed unwanted docs (801496c) (@Christiantyemele)
- Lifecycle: Refine handling of blocked state and clarify transition rules (5d3ed29) (@Christiantyemele)
- Lifecycle: Invalidate obsolete reviews and recover rejected merges (7743392) (@NkwaTambe)
- Lifecycle: Enforce reviewed state transitions and merge gates (b64c178) (@NkwaTambe)
- Merge: Withhold non-submit PRs without clearing ticket progress (dc87de5) (@Christiantyemele)
- Merge: Gate VESSEL on merge-ready PRs and hold non-submit PRs (bb8e7a5) (@Christiantyemele)
- Nexus: Stop stale reviewer before releasing its slot (3ff8c2c) (@NkwaTambe)
- Nexus: Restore rework PR under the rework ticket id (53eb12c) (@Christian Yemele)
- Nexus: Only restore rework PRs for provisionable tickets and match directive PR (c49f41d) (@Christian Yemele)
- Nexus: Skip escalated tickets and restore PR branch for rework provisioning (d1d28dc) (@Christian Yemele)
- Nexus: Find rework directives on InProgress tickets and verify forge chat liveness (2cc1b89) (@Christian Yemele)
- Nexus: Keep rework directive until chat created; stop escalated PR reentry (dfbe6d2) (@Christian Yemele)
- Review: Enforce SENTINEL role on review submit and validate verdict PR (226458e) (@Christian Yemele)
- Routing: Enhance task completion handling and recovery from audit trail (c60929a) (@Christiantyemele)
- State Machine: Refine lifecycle transitions and improve testing approval handling (826ee77) (@Christiantyemele)
- Verification: Simplify execution and return failures to building (e8fa7ff) (@Christiantyemele)
- Verification: Hand off project environments and recover infrastructure failures (f572945) (@Christiantyemele)
- Verification: Reuse forge checkout and recover review execution (ca5729c) (@Christiantyemele)
- Vessel: Remove unused reqwest dependency (998b621) (@Chrstian Yemele)
- Vessel: Close review-gate gaps in timeout, fallback, and rework dispatch (4c81edc) (@Christian Yemele)
- Update review command documentation to clarify PR review verdict namespace (0fe04b3) (@Christian Yemele)

### Code refactoring

- Skills: Improve FORGE and SENTINEL planning and review processes (9660d85) (@Christiantyemele)

### Chores

- Nexus: Run rustfmt on rework provisioning code (07e59a5) (@Christian Yemele)
- Apply rustfmt formatting (d63cddf) (@Christian Yemele)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.8...openflows-1.6.0

## [1.5.8] - 2026-09-22

### Bug fixes

- Docker: Replace unreliable workflow_run trigger and eliminate QEMU bottleneck (20f2c2e) (@test)

### Documentation

- Manager: Comment service bootstrap and health paths (4c77e2e) (@NkwaTambe)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.7...openflows-1.5.8

## [1.5.7] - 2026-09-22

### Features

- Scaffold openflows manager service (d661c0e) (@NkwaTambe)

### Bug fixes

- Manager: Bound readiness check duration (db34c48) (@NkwaTambe)
- Harden manager readiness and shutdown (9c90125) (@NkwaTambe)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.6...openflows-1.5.7

## [1.5.5] - 2026-09-22

### Features

- Bootstrap: Remove vestigial admin-owned nexus workspace ([#205](https://github.com/The-AgenticFlow/openflows/pull/205)) (b036f41) (@NkwaTambe)

### Bug fixes

- Bootstrap: Provision tenant workspace under admin session user (ac2a1f8) (@Christiantyemele)
- Bootstrap: Refuse admin identity when tenant user cannot be resolved (96c76f4) (@Christiantyemele)
- Docker Compose: Add TF_PLUGIN_CACHE_DIR for shared Terraform provider caching (62c52f4) (@Christiantyemele)
- External Auth: Retrieve device flow via GET not POST (fbbb2cb) (@Christiantyemele)
- Hooks: Block quoted shell-wrapper lifecycle calls (review) (313577b) (@Christiantyemele)
- Hooks: Anchor coder lifecycle guard to real invocations (482db0a) (@Christiantyemele)
- Nexus: Preserve assigned fallback sentinel as review owner (51c90d1) (@Christiantyemele)
- Nexus: Enforce strict 1:1 sentinel pairing per forge slot (272002b) (@Christiantyemele)
- Nexus: Do not grant chat ownership on transient lookup failure (2a69db4) (@Christiantyemele)
- Nexus: Enforce single-owner fallback on shared sentinel chat (7c0161d) (@Christiantyemele)
- Nexus: Allow fallback/assigned sentinel to own shared chat during recovery (f1190df) (@Christiantyemele)
- Nexus: Enforce 1:1 forge-sentinel fleet pairing and parallel spawn (4b4b964) (@Christiantyemele)
- Resolve Version: Bound window to exclude later release tags (c2b03e3) (@Christian Yemele)
- Scripts: Add 'run' to reset-script controller instruction (90d733a) (@Christiantyemele)
- Templates: Export external-auth token into workspace env (be51565) (@Christiantyemele)

### Documentation

- Quickstart: Align OAuth link and secret rotation with tenant-owned workspaces (review) (2b23302) (@NkwaTambe)

### Code refactoring

- Auth: Use GitHub App external auth as the sole GitHub token source (3a08cb8) (@Christiantyemele)
- Run: Remove duplicate host-side controller run command (965f828) (@Christiantyemele)

### Chores

- Docs: Clean up .env.example by removing optional admin user configuration (73abcea) (@Christiantyemele)
- Format: Apply cargo fmt across changed files (ef87f42) (@Christiantyemele)
- Format: Improve code readability by adjusting formatting in bootstrap.rs (5eba32f) (@Christiantyemele)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.4...openflows-1.5.5

## [1.5.4] - 2026-09-18

### Bug fixes

- Resolve Version: Drop end_at upper bound to fix clock-skew tag miss (475311d) (@Christiantyemele)
- Resolve Version: Add grace period to end_at timestamp for clock skew (797b8e3) (@Christiantyemele)
- Resolve Version: Drop end_at upper bound to fix clock-skew tag miss (624a43d) (@Christiantyemele)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.3...openflows-1.5.4

## [1.5.3] - 2026-09-17

### Bug fixes

- Changelog: Stop release-plz crashing on placeholder commit id (1b49d1f) (@Christiantyemele)
- Changelog: Render valid GitHub mentions from commit author (41c09b3) (@Christiantyemele)
- Docker: Copy workspace source before cargo fetch (12b648c) (@Christiantyemele)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.2...openflows-1.5.3

## [1.5.2] - 2026-09-17

### Features

- Env: Make tenant repo the single source of truth ([#213](https://github.com/The-AgenticFlow/openflows/pull/213)) (b7248c9) (@NkwaTambe)

### Bug fixes

- Bootstrap: Warn when existing tenant workspace needs recreation (3634be3) (@NkwaTambe)
- Ci, tenant: Address review — resolver, tenant repo persist, owner-scoped warn (1353294) (@Christiantyemele)
- Controller: Fail fast when no target repository can be resolved (2e3f920) (@NkwaTambe)

### Documentation

- Arch: Describe current repo source without past-comparison (review) (fe741ba) (@NkwaTambe)
- Soften multi-tenant claim and reduce DEV/DEBUG repetition (review) (02d985d) (@NkwaTambe)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.5.1...openflows-1.5.2

## [1.5.0] - 2026-09-17

### Features

- Debug: Add workspace provider function with fallback for debug output (4f5f592) (@Christiantyemele)

### Bug fixes

- Changelog: Render clean commit lines without links or bodies (f95dd21) (@Christiantyemele)

### Continuous integration

- Fix zizmor workflow_run check on docker-publish (e36cb8e) (@Christiantyemele)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.4.0...openflows-1.5.0

## [1.4.0] - 2026-09-17

### Features

- Debug: Surface compiled binary version in debug output (fc12160) (@v)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.3.3...openflows-1.4.0

## [1.3.3] - 2026-09-17

### Documentation

- Readme: Add GHCR image link (4639eed) (@v)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-1.3.2...openflows-1.3.3

## [1.3.0] - 2026-09-16

### Features

- Coder: Pin Coder to v2.37.0 and validate version in doctor ([#186](https://github.com/The-AgenticFlow/openflows/pull/186)) (2cae85c) (@AgentFlow)
- Coder Client: Migrate Chats API to /api/v2 ([#186](https://github.com/The-AgenticFlow/openflows/pull/186)) (3ad6b85) (@AgentFlow)
- Config: Centralize remaining env reads incl GITHUB_API_BASE (#185, #204) (a8b4f8c) (@AgentFlow)
- Config: Centralize environment configuration with envconfig ([#185](https://github.com/The-AgenticFlow/openflows/pull/185)) (266d517) (@AgentFlow)
- Docker: Update Coder image version and add lifecycle hook configuration (bac1291) (@AgentFlow)
- Hooks: Implement hook-driven state derivation and behavior triggering (05ad1a4) (@AgentFlow)
- Hooks: Add standalone hook consumer for local testing (427f74b) (@AgentFlow)
- Hooks: Consume Coder agent-lifecycle-hooks (experimental) (87d80ef) (@AgentFlow)
- Hygiene: Remove migration doc (7063b5f) (@Christiantyemele)
- Prefer GitHub PAT for workspace git, fix repo acquisition (41350ae) (@AgentFlow)

### Bug fixes

- Coder: Use Coder external-auth token for workspace git access (7bcd407) (@NkwaTambe)
- Coder: Bump image tag default to v2.37.1 for GitHub App auth (fixes #211) (37f8dd9) (@NkwaTambe)
- Coder Client: Reject non-stream paths in mock chat server (d1e740f) (@AgentFlow)
- Coder Client: Use correct chat stream endpoint and resolve org earlier (09fadbd) (@AgentFlow)
- Config: Address PR review feedback on env centralization (5464931) (@AgentFlow)
- Config: Address review feedback on env centralization ([#204](https://github.com/The-AgenticFlow/openflows/pull/204)) (4fbea6d) (@NkwaTambe)
- Config: Address PR review feedback ([#204](https://github.com/The-AgenticFlow/openflows/pull/204)) (b3d0307) (@AgentFlow)
- Doctor: Require external-auth client ID; centralize via CoderConfig ([#219](https://github.com/The-AgenticFlow/openflows/pull/219)) (b92c90e) (@NkwaTambe)
- Doctor: Only compare semantic-version Coder tags, skip floating tags (b8caa4b) (@AgentFlow)
- Doctor, bootstrap: Note external auth as needed for agent auth (37a9021) (@NkwaTambe)
- Hooks: Close argv probe bypass and wrapper-argument write evasion (b50e3b8) (@AgentFlow)
- Hooks: Close blocked-probe write bypass and RUSTSEC-2023-0071 (bba5117) (@AgentFlow)
- Hooks: Harden guard against wrapped/segmented write evasion and compound replan bypass (47ee7c0) (@AgentFlow)
- Hooks: Harden phase guard against write/phase bypasses (9b3b727) (@AgentFlow)
- Hooks: Propagate OPENFLOWS_HOOK_URL to consumer and document workspace recreation (b5ffc58) (@AgentFlow)
- Hooks: Make CODER_CHAT_HOOK_URL the single source of truth for aud (7f86e5f) (@AgentFlow)
- Hooks: Classify shell writes by actual target, not artifact name (c1e83e2) (@AgentFlow)
- Hooks: Harden lifecycle hooks per review feedback (7053f95) (@AgentFlow)
- Hooks: Accept Coder's deployment-ID issuer in hook JWT (22de204) (@AgentFlow)
- Lifecycle Hooks: Recover hook orchestration and replay pending events (04be0f2) (@AgentFlow)
- Nexus: Default coder_url to localhost:7080 in coder_client_from_store (16a32d4) (@NkwaTambe)
- Startup: Address review findings on cleanup, count, and worker auth (1d0e96c) (@NkwaTambe)
- Startup: Simplify quick-start and fix onboarding bugs (293b690) (@NkwaTambe)
- Derive release version safely on manual dispatch of release-assets (60a35b4) (@AgentFlow)

### Documentation

- Architecture: Record Coder v2.37.0 GA migration findings (bdf74e3) (@NkwaTambe)
- Coder: Document git-credential store tradeoff vs GIT_ASKPASS ([#220](https://github.com/The-AgenticFlow/openflows/pull/220)) (6d74cf0) (@NkwaTambe)
- Config: Update env audit to match external auth field names ([#204](https://github.com/The-AgenticFlow/openflows/pull/204)) (3a4d4f3) (@AgentFlow)
- Quick Start: Focus guide on operator setup for hooks and GitHub App (f129ff7) (@AgentFlow)
- Quick Start: Address Greptile review on GitHub App setup (383908a) (@NkwaTambe)
- Quick Start: Document GitHub App permissions and installation-approval (3d1fd22) (@NkwaTambe)
- Quick Start: Document linking the GitHub App for private-repo access (0d88b8c) (@NkwaTambe)
- Quick Start: Verify GitHub App via deployment external-auth page after sign-in (0b87e99) (@NkwaTambe)
- Quick Start: External auth block is already uncommented; just set values (a07ee2e) (@NkwaTambe)
- Quick Start: Simplify install URL to concrete example (23494cc) (@NkwaTambe)
- Quick Start: Explain how to find the GitHub App slug (c53e5ea) (@NkwaTambe)
- Quick Start: Point to the redirect URI field as the callback (9dabf90) (@NkwaTambe)
- Quick Start: Drop GitHub App nav path; clarify Homepage URL field (b5c1a29) (@NkwaTambe)
- Quick Start: Rewrite quickstart with GitHub App auth; lowercase all .md docs (a0937b5) (@NkwaTambe)
- Address review — fix WS stream endpoint, pinning and §8.4 ref (b9b0cc5) (@AgentFlow)
- Repair dangling links after architecture doc restructure (07cf801) (@AgentFlow)
- Restructure architecture docs and add GitHub external auth config (17d3c51) (@AgentFlow)

### Tests

- Coder Client: Cover v2 chat paths ([#186](https://github.com/The-AgenticFlow/openflows/pull/186)) (704fc47) (@AgentFlow)

### Chores

- Coder: Update non-code refs to org-scoped v2 models ([#186](https://github.com/The-AgenticFlow/openflows/pull/186)) (00ae71d) (@AgentFlow)
- Doctor: Wrap long println to satisfy rustfmt (baf7855) (@NkwaTambe)
- Apply rustfmt formatting (a3f91e0) (@AgentFlow)
- Finalize single-bundle release from develop to main (ca3377f) (@AgentFlow)
- Revert to single openflows release without orchestration bundling (f250a29) (@AgentFlow)

**Full Changelog**: https://github.com/The-AgenticFlow/openflows/compare/openflows-harness-1.2.1...openflows-1.3.0

## [harness-1.2.1] - 2026-08-24

### Features

- Lore: Commit and push docs PR after generating documentation (8c804ef) (@Christian Yemele)
- Lore: Commit and push docs PR after generating documentation (ad7668a) (@Christian Yemele)
- Model Discovery: Dynamically discover Anthropic models from API (fe921a6) (@AgentFlow)
- Nexus: Add real E2E test, mcp-proxy bridge, and enhanced logging (2e37d31) (@Christian Yemele)
- Nexus: Add real E2E test, mcp-proxy bridge, and enhanced logging (23c9cc5) (@Christian Yemele)
- Phase2: Implemented nexus (c9caf36) (@Christian Yemele)
- Phase2: Implemented nexus (f6033f0) (@Christian Yemele)
- Setup: Auto-detect and set CLI binary path in .env (4600e65) (@AgentFlow)
- TUI: Redesign agent config screen with inline editing (9a8d93c) (@AgentFlow)
- TUI: Add agent configuration step for instances, model backend, and tokens (8697c43) (@AgentFlow)
- Split releases into openflows and openflows-harness without v prefix (a54093d) (@AgentFlow)
- Run releases only on pushes to main (0252404) (@AgentFlow)
- Auto-version releases from conventional commits (7070657) (@AgentFlow)
- Adopt release-plz with develop-as-default branching (7a75b43) (@AgentFlow)
- Openflows x coder (5451033) (@AgentFlow)
- Add tenant clean command and gate approval system (b0a06fc) (@AgentFlow)
- Add dev-sync script and auto-sync binary in bootstrap (5c39750) (@AgentFlow)
- Include full agent persona in initial chat prompt (3aa4602) (@AgentFlow)
- Orchestrator Coder agent integration fixes (0f6da42) (@AgentFlow)
- Implement Coder integration plan (Phases 1-3) (1f6d2bf) (@AgentFlow)
- Include issue number in forge PR prompts to link pull requests automatically (c34246b) (@AgentFlow)
- Embed all orchestration files at compile time with self-healing and version tracking (bdc2c2e) (@AgentFlow)
- Auto-release on push to main with stable/edge channels (3d8d873) (@AgentFlow)
- Add domain configuration step to interactive setup (92c6451) (@AgentFlow)
- Add detailed prerequisites to README, refactor output indexing in responses proxy, extract strip_provider_prefix to agent-client (f710aee) (@AgentFlow)
- Packaging and distribution setup (edb0dd2) (@AgentFlow)
- Separate GitHub and LLM provider setup into distinct steps (01ddadd) (@AgentFlow)
- Update GitHub username handling for issue assignment and add authenticated user retrieval (096de39) (@Ngha-Boris)
- Implement GitHub issue assignment, comment, and labeling functionality (ee971f1) (@Ngha-Boris)
- Add DEFAULT_CLI env var override for CLI backend selection (a479b45) (@ndefokou)
- Per-agent GitHub tokens with instance suffix support (02853f6) (@AgentFlow)
- Per-agent GitHub tokens, git identity from PAT, and REST client extensions (eb56025) (@AgentFlow)
- Auto-sync worktree branches with main on merge (d94b3b8) (@Christian Yemele)
- Add conflict resolver, enhance vessel/nexus agents, and improve proxy-first routing (1a420c5) (@Christian Yemele)
- Implement VESSEL agent (DevOps & Merge Gate) (9a17ad4) (@Christian Yemele)
- Add Docker infrastructure with LiteLLM proxy and agent-team service (4ee824b) (@Christian Yemele)
- Add model mapping and streaming support to anthropic-proxy (84a3dae) (@Christian Yemele)
- Complete FORGE-SENTINEL pair lifecycle with SENTINEL review certification (1f367fc) (@Christian Yemele)
- Switch real_test to use ForgePairNode for full SENTINEL lifecycle (d7d60aa) (@Christian Yemele)
- Implement FORGE-SENTINEL pair architecture v3.0 (b006d1c) (@Christian Yemele)
- Complete Phase 4 orchestration with robust JSON extraction and worker logging (0f0a8fb) (@Christian Yemele)
- Implement hosted MCP bridge, nexus E2E tests, and contributing guide (adfca87) (@Christian Yemele)
- Per-agent GitHub tokens with instance suffix support (30ca4e8) (@AgentFlow)
- Per-agent GitHub tokens, git identity from PAT, and REST client extensions (37d89ed) (@AgentFlow)
- Auto-sync worktree branches with main on merge (b36b91d) (@Christian Yemele)
- Add conflict resolver, enhance vessel/nexus agents, and improve proxy-first routing (3ca3294) (@Christian Yemele)
- Implement VESSEL agent (DevOps & Merge Gate) (001dad9) (@Christian Yemele)
- Add Docker infrastructure with LiteLLM proxy and agent-team service (cb117d4) (@Christian Yemele)
- Add model mapping and streaming support to anthropic-proxy (70885c4) (@Christian Yemele)
- Complete FORGE-SENTINEL pair lifecycle with SENTINEL review certification (4b0f1db) (@Christian Yemele)
- Switch real_test to use ForgePairNode for full SENTINEL lifecycle (ceb4aef) (@Christian Yemele)
- Implement FORGE-SENTINEL pair architecture v3.0 (145f4b0) (@Christian Yemele)
- Complete Phase 4 orchestration with robust JSON extraction and worker logging (1ea4c81) (@Christian Yemele)
- Implement hosted MCP bridge, nexus E2E tests, and contributing guide (6651837) (@Christian Yemele)

### Bug fixes

- A2A: Add pair-scope ownership check to tasks/cancel (d3a69a8) (@AgentFlow)
- A2A: Complete verify task transport end-to-end (091b6bb) (@AgentFlow)
- A2A: Declare MIT license (fixes cargo-deny license check) (4fc1832) (@AgentFlow)
- Agent Forge: Address PR review edge cases in denylist and validation (0e1874c) (@AgentFlow)
- Agent Forge: Address PR review feedback on configuration leak prevention (65ba572) (@AgentFlow)
- Agent Forge: Prevent sensitive agent configuration leaks (6cacbc4) (@AgentFlow)
- Deny: Add Zlib license to allow list (used by foldhash dependency) (02a661a) (@AgentFlow)
- Deny: Allow NOASSERTION license for workspace crates (3e6a341) (@AgentFlow)
- Deny: Add license exception for agentflow-tui workspace crate (1011e0b) (@AgentFlow)
- Deny: Remove invalid clarify section, keep private.ignore (ef39783) (@AgentFlow)
- Deny: Remove deprecated unlicensed key, use private.ignore (c10976c) (@AgentFlow)
- Deny: Set unlicensed=allow for workspace crates without license files (c240be6) (@AgentFlow)
- Deny: Ignore private workspace crates in license check (ca5e68e) (@AgentFlow)
- Docker: Copy orchestration/ directory in build stage (3b475a6) (@AgentFlow)
- Docs: Enhance bootstrap instructions with default admin credentials and password requirements (84f1f1d) (@Ngha-Boris)
- Docs: Update Step 4 in QUICK_START for clarity on Coder sign-in and API token retrieval (4b926f4) (@Ngha-Boris)
- Docs: Update QUICK_START and README for clarity and structure (361b0af) (@Ngha-Boris)
- Docs: Clarify controller log verification process in README (1440f26) (@Ngha-Boris)
- Docs: Add optional Coder admin user configuration and clarify tenant command syntax in README (1b0c4da) (@Ngha-Boris)
- Docs: Update Coder session token placeholder in .env.example for clarity (12d1cf4) (@Ngha-Boris)
- Docs: Update tenant command syntax and improve log verification instructions in README (7c1e9f2) (@Ngha-Boris)
- Docs: Update API token instructions and clarify Coder license creation process in README (eb5ddac) (@Ngha-Boris)
- Docs: Clarify instructions for copying the API token in README (76b43ca) (@Ngha-Boris)
- Docs: Update environment configuration in .env.example and docker-compose.yml for clarity (1297d1e) (@Ngha-Boris)
- Docs: Update Docker command in README to run in detached mode and clarify log monitoring instructions (411d530) (@Ngha-Boris)
- Docs: Update Docker command in README for clarity (110e91c) (@Ngha-Boris)
- Fallback: Remove Gemini from provider chain, keep only 3 supported providers (3671efc) (@AgentFlow)
- Forge: Handle STATUS.json parsing and success outcomes (79488a9) (@Christian Yemele)
- Forge: Claude Code stdin prompt and workspace isolation (2945d3f) (@Christian Yemele)
- Forge: Handle STATUS.json parsing and success outcomes (32cc161) (@Christian Yemele)
- Forge: Claude Code stdin prompt and workspace isolation (7d9d9cb) (@Christian Yemele)
- Install: Skip harness when no release exists in the selected channel (cad3db5) (@AgentFlow)
- Install: Honor musl fallback extraction dir and drop invalid gh --arg queries (e561f5e) (@AgentFlow)
- Install: Resolve release tags per package and install harness from its own archive (2efcc09) (@AgentFlow)
- Install: Check for either Claude Code or Codex, don't auto-install (9297adf) (@AgentFlow)
- Lore: Branch docs PRs from origin/main instead of forge branch (e0446cc) (@AgentFlow)
- Lore: Route merged PRs to lore before CI fixes (dc05c30) (@Christian Yemele)
- Lore: Branch docs PRs from origin/main instead of forge branch (fa1b272) (@AgentFlow)
- Lore: Route merged PRs to lore before CI fixes (7fa9f66) (@Christian Yemele)
- MCP: Inject BEARER_TOKEN via Command::env() to fix Bearer prefix (2d0ebfe) (@Christian Yemele)
- MCP: Inject BEARER_TOKEN via Command::env() to fix Bearer prefix (78cb72e) (@Christian Yemele)
- Model Discovery: Validate Anthropic model IDs, reject invalid API names (cfb121c) (@AgentFlow)
- Model Discovery: Use verified working Anthropic model IDs (cb33cdd) (@AgentFlow)
- Model Discovery: Use valid Anthropic API model IDs, not CLI aliases (c477b4a) (@AgentFlow)
- Nexus: Detect and re-spawn orphaned sentinel chats (1eba9ff) (@AgentFlow)
- Nexus: Move FORGE planning-gate notification into polling loop (e518946) (@AgentFlow)
- Nexus: Ensure sentinel spawns for planning gates, add forge gate-wait protocol (05b3577) (@AgentFlow)
- Nexus: Rotate forge chats bound to stale/dead workspaces (b351ab2) (@AgentFlow)
- Orchestration: Check Assigned tickets for planning gate and review_ready (d91f5e2) (@AgentFlow)
- Orchestration: Spawn SENTINEL for planning-gate review (2443ae5) (@AgentFlow)
- Pair Harness: Add ticket_id field to PairConfig for per-issue scoping (4b8b1c2) (@Christian Yemele)
- Pair Harness: Add ticket_id field to PairConfig for per-issue scoping (4119b32) (@Christian Yemele)
- Proxy: Add /v1/models endpoint for Claude Code model validation (6280c74) (@AgentFlow)
- Sentinel: Consume review verdict after processing, fix forge chat lookup (e009144) (@AgentFlow)
- Setup: Ensure CLI backend is properly set in registry.json (09817be) (@AgentFlow)
- Setup: Add GITHUB_MCP_CMD to .env file (029746a) (@AgentFlow)
- Templates: Escape ${HARNESS_ASSET} in startup heredoc so Terraform plans (cfb7872) (@AgentFlow)
- Templates: Make workspace harness install robust; fix repo URL (733e863) (@AgentFlow)
- Templates: Wire workspaces to the nexus A2A relay (0456e56) (@AgentFlow)
- Templates: Correct $$ escaping in sentinel/lore startup scripts (7181587) (@AgentFlow)
- Templates/sentinel: Add shared orchestration volume mount (c571059) (@AgentFlow)
- Templates/sentinel: Wire coder_parameter blocks (ticket_id, tenant, coder_url) (1070213) (@AgentFlow)
- TUI: Improve setup flow order and add edit option (6482ac5) (@AgentFlow)
- TUI: Lock Nexus instances, fix GitHub token propagation, anchor artifacts to ~/.agentflow (020786e) (@AgentFlow)
- TUI: Make current_state mutable in agent step (1d6ab53) (@AgentFlow)
- Vessel: Detect merge conflicts in merge result and route to conflict handler (82c8127) (@AgentFlow)
- Vessel: Detect merge conflicts in merge result and route to conflict handler (860345c) (@AgentFlow)
- Verify zig toolchain SHA-256 on install (38d1d39) (@AgentFlow)
- Report guard check as validate-release-branch to match main protection (732ec4a) (@AgentFlow)
- Authenticate build toolchain and enforce release branch shape (abcdc68) (@AgentFlow)
- Address gitar PR review on release workflows (041c0d5) (@AgentFlow)
- Harden release-source provenance and main scoping (88c5187) (@AgentFlow)
- Scope workflow permissions to job level for zizmor (bae56fe) (@AgentFlow)
- Avoid shell injection in PR title lint step (50b7a17) (@AgentFlow)
- Address review feedback on title lint and manual release (60efcb4) (@AgentFlow)
- Use ESM export in commitlint config (b011613) (@AgentFlow)
- Scope release-plz packages and repair CI workflows (1bd66d3) (@AgentFlow)
- Scope workflow permissions to job level for zizmor (7b41ead) (@AgentFlow)
- Bump h2 to 0.4.16 to fix RUSTSEC-2026-0258 (e4f7721) (@AgentFlow)
- Reuse existing CODER_SESSION_TOKEN instead of hardcoding admin (934d8ee) (@AgentFlow)
- Resolve active user from session token instead of hardcoding admin (8d038c6) (@AgentFlow)
- Improve quick-start UX, default env vars, and GitHub sign-ups (025e3e4) (@AgentFlow)
- Relay cancel_task keeps Cancelled state so executor poller detects it (e5b1c03) (@AgentFlow)
- Move plan storage to Redis SharedStore, replace FORGE polling with NEXUS notification (bb7357e) (@AgentFlow)
- Remove needless borrow flagged by clippy -D warnings (841b109) (@AgentFlow)
- Recycle dead chats on error and prevent stale binaries from silently running (6959f60) (@AgentFlow)
- Prevent infinite workspace recreation loop when status is unknown (0fc4218) (@AgentFlow)
- Wait for agent to be fully Ready, not just Starting (7f147b6) (@AgentFlow)
- Add confirmation step before starting controller (d327a1a) (@AgentFlow)
- Add start_controller parameter to prevent auto-start during bootstrap (73e7469) (@AgentFlow)
- Controller reports 'no assignable work' despite tickets in repository (5ba8d59) (@AgentFlow)
- Include openflows-harness in dev-sync binary sync (c9bd9a2) (@AgentFlow)
- Update anyhow to v1.0.104 to fix undefined behavior advisory (RUSTSEC-2024-0436) (df0b74c) (@AgentFlow)
- Track orchestration files and fix clippy errors (5a519ec) (@AgentFlow)
- Agent orchestration pipeline - 6 critical bugs (3b1322c) (@AgentFlow)
- Cycle nexus→forge_pair while workers are busy, not stop (cd300b5) (@AgentFlow)
- Break nexus<->forge_pair infinite loop and fix v2 registry slot derivation (75dbaa2) (@AgentFlow)
- Surface FORGE startup errors on first rapid exit and quiet poller log noise (b956154) (@AgentFlow)
- Connect Coder workspaces to compose network and fix agent download URL (ae761a9) (@AgentFlow)
- Use ghcr.io/coder/coder image instead of non-existent coder/coder (26cb7df) (@AgentFlow)
- Use --env-file /dev/null for docker-compose to avoid parsing project .env (92a4430) (@AgentFlow)
- Add Coder bootstrap and docker-compose auto-start to running binary (dac3c02) (@AgentFlow)
- Address PR review — filter duplicate --output-format and apply cargo fmt (88084d1) (@AgentFlow)
- Sentinel --sandbox flag for Claude backend + inotify fallback (1b50e3f) (@AgentFlow)
- Omit issue linking tag from PR prompt when issue number is zero (39ce623) (@AgentFlow)
- Address PR review feedback — prevent silent identity fallback and comment spam (469359d) (@AgentFlow)
- Use dynamic token-based GitHub identity resolution for issue assignment (995f966) (@AgentFlow)
- Make crates.io publish tolerant of already-published crates (351cf07) (@AgentFlow)
- Vessel node now uses resolver registry and searches OPENFLOWS_HOME first (2e342e0) (@AgentFlow)
- Report base registry id in inactive-agent error message (ad0c049) (@Gitar)
- Apply rustfmt formatting to registry.rs (c6cf502) (@Gitar)
- Also check suffix-stripped base id when detecting inactive agents in resolve_github_token (282a8ea) (@Gitar)
- Comprehensive defense against registry/token/path bugs (3b56058) (@AgentFlow)
- Reject candidates inside orchestration/agent to prevent doubled paths (d1cedb9) (@AgentFlow)
- Address PR reviews — backup from OPENFLOWS_HOME, extract helper, add reset recovery hint (1de1ca7) (@AgentFlow)
- Proper indentation for install.sh registry backup blocks (78d1f1f) (@AgentFlow)
- Preserve user registry.json during install and preserve on reset (3a088cf) (@AgentFlow)
- Prioritize OPENFLOWS_HOME registry and gracefully skip lore on token error (08122fd) (@AgentFlow)
- Use cargo-zigbuild with glibc 2.17 for gnu targets (c83fcbb) (@AgentFlow)
- Remove unsafe musl→gnu fallback and add aarch64 cross-linker env var (4880db3) (@AgentFlow)
- Address PR review feedback — glibc compat and aarch64-musl fallback (48ad95c) (@AgentFlow)
- Add x86_64-unknown-linux-gnu release target and musl fallback in install script (588ca4c) (@AgentFlow)
- Set executable permissions on .sh files and restore trap in install.sh (522cd6a) (@AgentFlow)
- Address gitar review feedback (0ce12b4) (@AgentFlow)
- Track all orchestration files in git and remove duplicate profile (505d114) (@AgentFlow)
- Update 'cd AgentFlow' to 'cd openflows' in all docs (fadda83) (@AgentFlow)
- Update repo URL from AgentFlow to openflows and make install copies orchestration/ (a58a012) (@AgentFlow)
- Make install copies orchestration/ and document cargo install caveat (231321c) (@AgentFlow)
- OPENFLOWS_HOME used directly without double-appending /.openflows in load_env() (e3783f0) (@AgentFlow)
- Standardize on OPENFLOWS_HOME, propagate .env parse errors, remove /tmp fallback (5ae940b) (@AgentFlow)
- Load .env from ~/.openflows, install orchestration config, and improve Quick Start (5b120b0) (@AgentFlow)
- Use unscoped npm package name and remove non-existent optionalDeps (ebda84b) (@AgentFlow)
- Use edge docker tag for prereleases and jq for edge tag resolution (5071a1d) (@AgentFlow)
- Exclude dev tags from stable tag lookup in release workflow (c4221d0) (@AgentFlow)
- Address all PR review feedback (73a9b27) (@AgentFlow)
- Strip provider prefix from FIREWORKS_MODEL and OPENAI_MODEL env var fallbacks (02fe665) (@AgentFlow)
- Drain MCP stderr in background task and use valid fallback model (9ed499d) (@AgentFlow)
- Tighten needs_host_push heuristic and correct agent-system docs (eea2ff7) (@AgentFlow)
- Resolve CI failures (fedc766) (@AgentFlow)
- Rewrite domain step with Layout constraints, richer descriptions (b1c07fa) (@AgentFlow)
- Break infinite CI fix loop — nexus guard, annotations enrichment, diagnostic logging (cb8cb93) (@AgentFlow)
- Create temp registry.json in nexus e2e test instead of reading from workspace (0a8af31) (@AgentFlow)
- Resolve nexus e2e test by using CARGO_MANIFEST_DIR for workspace-relative paths (a1acbaf) (@AgentFlow)
- Resolve CI failures (spelling, format) (64c9d1b) (@AgentFlow)
- Clippy warnings, format, and test fixes for CI (da26aaa) (@AgentFlow)
- Accept newer model ID formats, fix fireworks routing, update default model (1a86d07) (@AgentFlow)
- Strip provider prefix from model IDs before passing to CLI backends and API clients (0308335) (@AgentFlow)
- Override cli and model_backend from provider when loading existing registry (a07dd00) (@AgentFlow)
- Add back Anthropic support alongside OpenAI and Fireworks (d32ba0f) (@AgentFlow)
- MCP startup readiness detection, stream completion events, and worktree nesting guard (2874268) (@AgentFlow)
- Disable non-function tool types for SSE custom providers (2d4225d) (@AgentFlow)
- UTF-8 panic in response truncation and Codex v0.133.0 compatibility (7446433) (@AgentFlow)
- Tune snif review indexing timeout (eed6e53) (@Assah Bismark)
- Bump snif to v3.2.7 for provider rate limit fix (359025c) (@Assah Bismark)
- Use ubuntu-22.04 for gnu builds instead of cargo-zigbuild (f68b5aa) (@AgentFlow)
- Address PR review feedback — glibc compat and aarch64-musl fallback (81eba7c) (@AgentFlow)
- Add x86_64-unknown-linux-gnu release target and musl fallback in install script (21305c5) (@AgentFlow)
- Prefix unused config param with underscore (3c03365) (@AgentFlow)
- Generate registry.json in setup wizard, fix next steps commands (ed1fb3a) (@AgentFlow)
- Resolve orchestration files relative to binary in agentflow.rs (48d6599) (@AgentFlow)
- Resolve orchestration files relative to binary location (167edca) (@AgentFlow)
- Point JS wrappers to agentflow binaries (dc96ce6) (@AgentFlow)
- Add musl fallback for x86_64 Linux binary download (039606a) (@AgentFlow)
- Replace unstable floor_char_boundary with stable char_indices (b07cd7e) (@AgentFlow)
- Update Rust to 1.88 for instability crate requirement (7f64639) (@AgentFlow)
- Update Rust to 1.85 for edition2024 support, fix package name (c44d182) (@AgentFlow)
- Use rustls instead of native-tls in anthropic-mock (6907e77) (@AgentFlow)
- Add OpenSSL dev headers to Linux build, fix binary names in install script (6e1648c) (@AgentFlow)
- Add scrolling for GitHub PAT fields when they overflow screen (3fcd9f8) (@AgentFlow)
- Set input field height to 3 for proper border rendering (942a4ae) (@AgentFlow)
- Implementing plugin module for codex (04b5b67) (@ndefokou)
- Pass CLI backend from PairConfig to ProcessManager (2add61d) (@ndefokou)
- Use new_with_registry in real_test to pass CLI backend config (836756b) (@ndefokou)
- Add debug logging for CLI backend resolution (980135c) (@ndefokou)
- Propagate CLI backend from registry to PairConfig (8730b1f) (@ndefokou)
- Respect GITHUB_MCP_CMD in connect_hosted_with_token for test mocking (03e7471) (@AgentFlow)
- Resolve clippy warnings - implement FromStr trait and remove dead code (725648c) (@AgentFlow)
- Suppress unused variable warning in github client test (93c59bd) (@Christian Yemele)
- Resolve CI failures (format, clippy, license) (0b2dc8e) (@Christian Yemele)
- Close GitHub issues on merge and fix worktree path mismatch (c64053b) (@Christian Yemele)
- Resolve CI fix rework loop — FORGE re-enters implementation mode instead of fixing CI failures (a1a56bf) (@Christian Yemele)
- Set release-type major for semver-checks (0.x breaking changes allowed) (1f090d7) (@Christian Yemele)
- Resolve semver-checks by setting release-type minor and bumping crate versions (0f55e12) (@Christian Yemele)
- Collapse match guard in agent-nexus for clippy (35e6700) (@Christian Yemele)
- Resolve clippy, fmt, cargo-deny, and semver-checks CI failures (a6f2e9c) (@Christian Yemele)
- Worktree path resolution and force-with-lease stale info (c11168d) (@Christian Yemele)
- Resolve merge conflict infinite loop — unrelated histories, force-push, duplicate PR prevention (05ea9ce) (@Christian Yemele)
- Resolve FORGE-SENTINEL lifecycle failures preventing PR creation and VESSEL invocation (c269a90) (@Christian Yemele)
- Simplify env config - proxy mode doesn't require individual API keys (3bdcde6) (@Christian Yemele)
- Handle empty STATUS.json race in read_status to prevent sentinel review EOF parse error (7b11f53) (@Christian Yemele)
- Add ticket state machine, remove nested tokio runtime, and fix worktree cleanup (ffc0dc3) (@Christian Yemele)
- Add debounce, SENTINEL tracking, and output logging to prevent system hangs (8d05417) (@Christian Yemele)
- Improve NEXUS persona to clarify worker_slots format (700f609) (@Christian Yemele)
- STATUS.json parsing and nexus issue discovery (bcded95) (@Christian Yemele)
- Respect GITHUB_MCP_CMD in connect_hosted_with_token for test mocking (e25444f) (@AgentFlow)
- Resolve clippy warnings - implement FromStr trait and remove dead code (5398d0b) (@AgentFlow)
- Suppress unused variable warning in github client test (103baae) (@Christian Yemele)
- Resolve CI failures (format, clippy, license) (26eb85c) (@Christian Yemele)
- Close GitHub issues on merge and fix worktree path mismatch (a59f93b) (@Christian Yemele)
- Resolve CI fix rework loop — FORGE re-enters implementation mode instead of fixing CI failures (dee2949) (@Christian Yemele)
- Set release-type major for semver-checks (0.x breaking changes allowed) (30f40eb) (@Christian Yemele)
- Resolve semver-checks by setting release-type minor and bumping crate versions (1e787b3) (@Christian Yemele)
- Collapse match guard in agent-nexus for clippy (3d1f54b) (@Christian Yemele)
- Resolve clippy, fmt, cargo-deny, and semver-checks CI failures (7b73c94) (@Christian Yemele)
- Worktree path resolution and force-with-lease stale info (c02980a) (@Christian Yemele)
- Resolve merge conflict infinite loop — unrelated histories, force-push, duplicate PR prevention (657fd83) (@Christian Yemele)
- Resolve FORGE-SENTINEL lifecycle failures preventing PR creation and VESSEL invocation (cae0b91) (@Christian Yemele)
- Simplify env config - proxy mode doesn't require individual API keys (811fc39) (@Christian Yemele)
- Handle empty STATUS.json race in read_status to prevent sentinel review EOF parse error (2d3d4cd) (@Christian Yemele)
- Add ticket state machine, remove nested tokio runtime, and fix worktree cleanup (61f97f1) (@Christian Yemele)
- Add debounce, SENTINEL tracking, and output logging to prevent system hangs (852b9ec) (@Christian Yemele)
- Improve NEXUS persona to clarify worker_slots format (cf60121) (@Christian Yemele)
- STATUS.json parsing and nexus issue discovery (c5f556f) (@Christian Yemele)

### Documentation

- A2A: Document v1 pair-scope trust model honestly (540f89a) (@AgentFlow)
- Readme: Expand npm installation and usage instructions (3021b19) (@AgentFlow)
- Keep organization-workspace-access in edit-roles example (4044ab2) (@AgentFlow)
- Enhance quick start guide with OAuth user permissions and roles (1194909) (@AgentFlow)
- Clarify that bootstrap builds both openflows and openflows-harness (5184141) (@AgentFlow)
- Clarify Quick Start order - add tenant before starting controller (5408479) (@AgentFlow)
- Remove autonomous mode — CLI mode only for OpenFlows agents (09ac070) (@AgentFlow)
- Switch default model routing to AI Gateway (Premium license available) (8e9f5df) (@AgentFlow)
- Add ephemeral Coder workspace integration plan (2838e55) (@AgentFlow)
- Add article — OpenFlows and Coder: The Missing Piece (2707d42) (@AgentFlow)
- Add OpenFlows x Coder integration architecture design doc (604047b) (@AgentFlow)
- Update CLI backend docs and add domain config env vars (62b6282) (@AgentFlow)
- Clarify agent system — add harness system section, concrete examples, and extension path guidance (3d5898c) (@AgentFlow)
- Link contribution_guidelines.md from CONTRIBUTING.md (30c7007) (@micheal-ndoh)
- Add contribution_guidelines.md for branching, commits and PR standards (e88eb82) (@micheal-ndoh)
- Refactor README - separate installation guide into INSTALL.md (c2b726e) (@NkwaTambe)
- Update Discord link to correct community URL (964908e) (@Christian Yemele)
- Improve README with clear project description and autonomous workflow (8824da7) (@Christian Yemele)
- Refactor CONTRIBUTING.md for better contributor onboarding (9cb7e40) (@Christian Yemele)
- Update SharedStore documentation with TUI monitoring interface details and enhance architecture overview (b0d0104) (@Ngha-Boris)
- Update SharedStore documentation with TUI monitoring interface details and enhance architecture overview (8b93cbd) (@Ngha-Boris)
- SharedStore documentation with detailed architecture overview and API reference (18d82d4) (@Ngha-Boris)
- Add CLI backend configuration documentation (a2dd2a4) (@ndefokou)
- Clarify environment configuration for contributors (1060997) (@Christian Yemele)
- Rewrite demo.md as contributor walkthrough, link from README and CONTRIBUTING (12af42e) (@Christian Yemele)
- Clarify STATUS.json location and SENTINEL evaluation workflow (d23dfc6) (@Christian Yemele)
- Add comprehensive tutorial, setup checker, and demo recording guide (9cb80be) (@Christian Yemele)
- Add comprehensive demo guide and update README (a24b76d) (@Christian Yemele)
- Add FORGE-SENTINEL architecture design (90d37eb) (@Christian Yemele)
- Remove design.md and update references (753461d) (@Christian Yemele)
- Move design.pdf to docs/ and update links (545423c) (@Christian Yemele)
- Update installation directory (4a715f3) (@JOELNATHAN544)
- Add contribution steps (design reading, test verification, and issue-based onboarding) (7b6b79f) (@Christian Yemele)
- Improve onboarding, setup instructions, and contribution guide (390bd8a) (@Christian Yemele)
- Clarify environment configuration for contributors (bcdfcdd) (@Christian Yemele)
- Rewrite demo.md as contributor walkthrough, link from README and CONTRIBUTING (b02408b) (@Christian Yemele)
- Clarify STATUS.json location and SENTINEL evaluation workflow (f68e0e9) (@Christian Yemele)
- Add comprehensive tutorial, setup checker, and demo recording guide (00ba93c) (@Christian Yemele)
- Add comprehensive demo guide and update README (7e9a4ab) (@Christian Yemele)
- Add FORGE-SENTINEL architecture design (5a71511) (@Christian Yemele)
- Remove design.md and update references (e755e6b) (@Christian Yemele)
- Move design.pdf to docs/ and update links (14acc9d) (@Christian Yemele)
- Update installation directory (378f300) (@JOELNATHAN544)
- Add contribution steps (design reading, test verification, and issue-based onboarding) (7adebee) (@Christian Yemele)
- Improve onboarding, setup instructions, and contribution guide (043760e) (@Christian Yemele)

### Code refactoring

- Rename orchestration volume to artifacts (44c2184) (@AgentFlow)
- Apply line wrapping to formatting strings and assertions for improved readability (0740297) (@AgentFlow)
- Call current_exe() once and derive parent dirs from single result (e8b1faa) (@AgentFlow)
- Reduce embedded files from 80 to 9 critical files (5d9b77c) (@AgentFlow)
- Simplify provider support to Codex + OpenAI/Fireworks only (8a99c96) (@AgentFlow)
- Rename all agentflow binaries to openflows (966d939) (@AgentFlow)
- Rewrite README to focus on project overview, not implementation details (aae02ba) (@AgentFlow)

### Tests

- Add codex E2E test variants for nexus (d2212da) (@AgentFlow)

### Continuous integration

- Fix action versions using tags instead of commit SHAs (5b90d06) (@AgentFlow)
- Add PR linter job validating titles and template sections (1b9a5c4) (@AgentFlow)
- Retry CI checks (332a56d) (@AgentFlow)

### Chores

- Sync Cargo.lock to 1.2.1 for openflows and openflows-harness (0ac905a) (@AgentFlow)
- Bump openflows and openflows-harness to 1.2.1 (bd1a946) (@AgentFlow)
- Trigger release-plz scan for dummy PR (42bb104) (@AgentFlow)
- Allowlist gitar spelling in typos config (419a982) (@AgentFlow)
- Add verify task schema and Redis key helpers (b5f484d) (@AgentFlow)
- V1.1.8 — fix worker workspaces booting without openflows-harness (cc673c6) (@AgentFlow)
- Apply rustfmt to openflows-harness crate (PR #141) (745421d) (@AgentFlow)
- Disable automatic release on main push, only on tags or manual dispatch (f05b769) (@AgentFlow)
- Remove snif code review workflow and config (c3c42f9) (@AgentFlow)
- Fix formatting and trailing whitespace (85d3ce2) (@AgentFlow)
- Bump version to 1.1.6 (ba1980c) (@AgentFlow)
- Resolve conflicts with main — keep ubuntu-22.04 gnu builds, remove zigbuild (404204b) (@AgentFlow)
- Fix formatting for CI (4dbad79) (@AgentFlow)
- Bump version to 1.0.16 (c3c8df3) (@AgentFlow)
- Add project config module, domain provisioning, and various refinements (25bff0f) (@AgentFlow)
- Shared dir in worktree, MCP timeouts, Claude provisioning, RUST_LOG support (5051a7a) (@AgentFlow)
- Update cargo lock and npm version (1acf7e7) (@AgentFlow)
- Simplify welcome page to clean minimal design (7be51cc) (@AgentFlow)
- Redesign OpenFlows welcome page with clean boxed logo (c6c72a4) (@AgentFlow)
- Redesign TUI with cyberpunk terminal aesthetic (e5366b0) (@AgentFlow)
- Apply cargo fmt formatting (fd51da6) (@AgentFlow)
- Delete auto-sync GitHub Action — local VESSEL sync is sufficient (0e5b958) (@Christian Yemele)
- Resolve conflicts with main, preserving all VESSEL agent and main features (4921f45) (@Christian Yemele)
- Align plugin structure with orchestration/ centralized architecture (c73a36e) (@ndefokou)
- Add LICENSE file (0dc1a92) (@Christian Yemele)
- Project cleanup, rename binary to agentflow, and update docs (453c69b) (@AgentFlow)
- Apply cargo fmt formatting (301c590) (@AgentFlow)
- Delete auto-sync GitHub Action — local VESSEL sync is sufficient (d80ffa2) (@Christian Yemele)
- Resolve conflicts with main, preserving all VESSEL agent and main features (035c6b6) (@Christian Yemele)
- Align plugin structure with orchestration/ centralized architecture (6d48802) (@ndefokou)
- Add LICENSE file (2876626) (@Christian Yemele)

### Reverts

- Restore all 80 embedded orchestration files (a8e16d9) (@AgentFlow)
- Restore scoped @the-agenticflow/openflows package name (6904ff9) (@AgentFlow)
- Keep registry.json domain defaults unchanged (2c0defe) (@AgentFlow)

