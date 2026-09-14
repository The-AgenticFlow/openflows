# Environment Configuration Audit

Issue #185 — centralize environment configuration with `envconfig`.

This document inventories every environment variable read by the Rust
workspace, classifies it, and records the recommended action. It accompanies
the new centralized `crates/config/src/env.rs` layer.

## Classification key

| Tag | Meaning |
|---|---|
| `in-use` | Read by Rust code and needed at runtime. |
| `required` | `in-use` and mandatory (startup fails if missing) — no default. |
| `duplicated` | Same value defaulted/read inline in more than one crate; the inline default should move to the config layer. |
| `legacy-alias` | Two env vars mean the same thing; one should be canonical, the other kept as a deprecated alias. |
| `stale` / `unused-in-rust` | Only referenced by `.env.example`/docker-compose, never read by Rust. |
| `internal` | Set by the code itself (`std::env::set_var`) and consumed internally; not user configuration. |

## Inventory

| Variable | Files (Rust) | Classification | Action |
|---|---|---|---|
| `CODER_URL` | coder-client, agent-sentinel, agent-nexus, agent-forge, binary | in-use · duplicated | Centralize default (`http://localhost:7080`) in `CoderConfig.url` |
| `CODER_SESSION_TOKEN` | coder-client, sentinel, nexus, forge, vessel, binary | required | `CoderConfig.session_token`; required in controller |
| `CODER_API_TOKEN` | sentinel, nexus, forge, vessel, binary/doctor | legacy-alias | **Excluded from centralized layer** (duplicate of `CODER_SESSION_TOKEN`); callers keep their inline fallback |
| `CODER_ADMIN_USERNAME` | coder-client/bootstrap | in-use | `CoderConfig.admin_username` (default `admin`) |
| `CODER_ADMIN_EMAIL` | coder-client/bootstrap | in-use | `CoderConfig.admin_email` (default `admin@openflows.dev`) |
| `CODER_ADMIN_PASSWORD` | coder-client/bootstrap | in-use | `CoderConfig.admin_password` (Option; no baked-in default — bootstrapper applies a secure fallback only when absent/weak) |
| `CODER_GITHUB_TOKEN` | agent-vessel/types | in-use | `CoderConfig.github_token` |
| `CODER_IMAGE_TAG` | binary/doctor | in-use | `CoderConfig.image_tag` (default `v2.37.1`) |
| `CODER_TRANSPORT_VERBOSE` | provisioner/transport | confusing | **Excluded from centralized layer** |
| `CODER_WORKSPACE_ID` | openflows-harness/store | confusing | **Excluded from centralized layer** (read inline with default only) |
| `CODER_EXTERNAL_AUTH_0_CLIENT_ID` | .env.example, docker-compose | in-use | `CoderConfig.external_auth_client_id` (canonical; `CODER_EXTERNAL_AUTH_0_ID` remains an inline read in binary/doctor) |
| `CODER_EXTERNAL_AUTH_0_CLIENT_SECRET` | .env.example, docker-compose | in-use | `CoderConfig.external_auth_client_secret` (canonical; `CODER_EXTERNAL_AUTH_0_SECRET` remains an inline read in binary/doctor) |
| `CODER_EXTERNAL_AUTH_0_ID` | .env.example, binary/doctor | in-use · inline | OIDC external-auth provider ID (inline read; not centralized) |
| `CODER_EXTERNAL_AUTH_0_TYPE` | .env.example | in-use · Coder-side | External-auth provider type (e.g. `github`); Coder config, not centralized |
| `CODER_EXTERNAL_AUTH_0_SCOPES` | .env.example | in-use · Coder-side | External-auth requested scopes (e.g. `repo`); Coder config, not centralized |
| `CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL` | .env.example | in-use · Coder-side | GitHub App install URL surfaced in the Coder UI; not centralized |
| `REDIS_URL` | binary, debug, doctor, harness | required (harness) | `InfraConfig.redis_url` (Option; `effective_redis_url()` default `redis://localhost:6379`; harness requires it) |
| `A2A_RELAY_ADDR` | agent-nexus/a2a, harness/a2a_client | in-use · duplicated | `InfraConfig.a2a_relay_addr` (default `127.0.0.1:3000`) |
| `OPENFLOWS_TENANT` | coder-client, pocketflow-core, nexus, harness, binary | required (controller/harness) | `TenantConfig.tenant` (Option; `effective_tenant()` default `default`; required in controller) |
| `OPENFLOWS_TICKET` | openflows-harness | required | `TenantConfig.ticket` |
| `OPENFLOWS_ROLE` | openflows-harness | required | `TenantConfig.role` |
| `OPENFLOWS_HOME` | agent-vessel, binary/orchestration | in-use | `TenantConfig.home` (default `~/.openflows`) |
| `OPENFLOWS_REGISTRY_PATH` | coder-client, nexus, binary | in-use · internal | `TenantConfig.registry_path` |
| `OPENFLOWS_REGISTRY_JSON` | coder-client, nexus, binary | in-use · internal | `TenantConfig.registry_json` |
| `OPENFLOWS_NEXUS_WORKSPACE_ID` | coder-client | in-use · internal | `TenantConfig.nexus_workspace_id` |
| `OPENFLOWS_NEXUS_WORKSPACE_NAME` | coder-client | in-use · internal | `TenantConfig.nexus_workspace_name` |
| `OPENFLOWS_NEXUS_API_TOKEN` | coder-client | in-use · legacy-alias | `TenantConfig.nexus_api_token`; reconcile with `NEXUS_CODER_API_TOKEN` |
| `NEXUS_CODER_API_TOKEN` | coder-client | legacy-alias | Merge into `OPENFLOWS_NEXUS_API_TOKEN` |
| `OPENFLOWS_TAR` | coder-client/build.rs | in-use | `TenantConfig.tar` (default `tar`) |
| `ARTIFACTS_DIR` | agent-nexus, binary | in-use · internal | `TenantConfig`/`AgentConfig` |
| `AGENTFLOW_WORKSPACE_ROOT` | agent-lore, agent-vessel | in-use · legacy-alias | `AgentConfig.workspace_root` (canonical) |
| `WORKSPACE_ROOT` | agent-nexus, agent-vessel | legacy-alias | `AgentConfig.legacy_workspace_root` (deprecated) |
| `HOME` / `USERPROFILE` | coder-client, nexus, vessel, binary | in-use (platform) | Use `dirs` semantics / `TenantConfig.openflows_home()` |
| `GITHUB_TOKEN` | agent-nexus | in-use | `GithubConfig.token` |
| `GITHUB_REPOSITORY` | coder-client, nexus, binary | required | `GithubConfig.repository` |
| `GITHUB_PERSONAL_ACCESS_TOKEN` | coder-client, agent-lore, vessel, config | required | `GithubConfig.personal_access_token`; `effective_token()` |
| `GITHUB_API_BASE` | github/rest | in-use | `GithubConfig.api_base` (default `https://api.github.com`); `GithubRestClient::new` resolves it via `GithubConfig::init_from_env()` |
| `USE_AI_GATEWAY` | config/registry | in-use | `AgentConfig.use_ai_gateway` (lenient string; `"true"`/`"1"` enabled via `use_ai_gateway_enabled()`, consistent with `registry::resolve_ai_gateway_enabled`) |
| `ROLE` | coder-client/bootstrap | in-use | `AgentConfig.role` |
| `OPENFLOWS_CREATE_NEXUS_WORKSPACE` | coder-client/bootstrap | in-use | `AgentConfig.create_nexus_workspace_enabled()` (lenient string; default enabled, only `"false"` disables) |
| `DEFAULT_CLI` | config/registry | in-use | keep (registry-specific) |
| `SLACK_WEBHOOK_URL` | notifier | in-use | **Excluded from centralized layer** (notifier config removed) |
| `DISCORD_WEBHOOK_URL` | notifier | in-use | **Excluded from centralized layer** (notifier config removed) |
| `WHATSAPP_ACCOUNT_SID` | notifier | in-use | **Excluded from centralized layer** (notifier config removed) |
| `WHATSAPP_API_KEY` | notifier | legacy-alias | **Excluded from centralized layer** (notifier config removed) |
| `WHATSAPP_AUTH_TOKEN` | notifier | in-use | **Excluded from centralized layer** (notifier config removed) |
| `WHATSAPP_FROM_PHONE` | notifier | in-use | **Excluded from centralized layer** (notifier config removed) |
| `WHATSAPP_TO_PHONE` | notifier | in-use | **Excluded from centralized layer** (notifier config removed) |
| `WHATSAPP_PHONE_NUMBER` | notifier | legacy-alias | **Excluded from centralized layer** (notifier config removed) |
| `TF_VAR_dev_binary_host_path` | coder-client | internal | keep (set by code for terraform) |
| `CODER_PG_PASSWORD` | — (docker-compose only) | stale / unused-in-rust | remove from Rust docs; keep in compose |
| `CODER_PORT`, `CODER_INTERNAL_PORT` | — (docker-compose only) | stale / unused-in-rust | remove from Rust docs |
| `CODER_OAUTH2_GITHUB_ALLOW_SIGNUPS` | — (docker-compose only) | stale / unused-in-rust | remove from Rust docs |
| `CODER_ACCESS_URL` | — (compose/scripts only) | stale / unused-in-rust | — |

## Removed / excluded from the centralized layer

The following config was intentionally left out of `config::env` in issue #185:

- **Notifier config** — `SLACK_WEBHOOK_URL`, `DISCORD_WEBHOOK_URL`, and all
  `WHATSAPP_*` vars: notifier config and its struct were removed entirely.
- **`CODER_API_TOKEN`** — duplicate of `CODER_SESSION_TOKEN`; excluded (existing
  callers keep their inline fallback).
- **`CODER_TRANSPORT_VERBOSE`** — excluded (inline read in provisioner only).
- **`CODER_WORKSPACE_ID`** — excluded (inline read with default only).

## Recommended cleanups (non-blocking, follow-up)

1. Promote `AGENTFLOW_WORKSPACE_ROOT` as canonical; drop `WORKSPACE_ROOT` over
   time (kept only as a deprecated alias for now).
2. Reconcile `OPENFLOWS_NEXUS_API_TOKEN` vs `NEXUS_CODER_API_TOKEN`.
3. Remove docker-compose-only vars from Rust-facing documentation.
4. ~~Migrate remaining inline `std::env::var` reads in the agent crates onto the
   centralized accessors (`CoderConfig`, `InfraConfig`, `TenantConfig`,
   `GithubConfig`, `AgentConfig`).~~ **Done** — all remaining reads that belong to
   a centralized variable now route through the `config::env` structs. The only
   reads left are documented exclusions (e.g. `CODER_API_TOKEN`,
   `CODER_TRANSPORT_VERBOSE`, `CODER_WORKSPACE_ID`, notifier vars, `ARTIFACTS_DIR`,
   `TF_VAR_dev_binary_host_path`, `HOME`/`USERPROFILE`), dynamic keys, and
   `OPENFLOWS_TAR` in `coder-client/build.rs` (build scripts can only use
   `[build-dependencies]`, so the central `config` crate is not reachable there).
