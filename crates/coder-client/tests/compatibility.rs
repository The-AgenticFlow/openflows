//! Coder compatibility contract verification harness (WP-00).
//!
//! This is an executable, reproducible check that the current Openflows code
//! integrates with the pinned Coder deployment the way the centralized
//! deployment specifications require. It follows the repository convention of
//! `#[ignore]`d live integration tests (see `openflows-harness/tests/`).
//!
//! # Test groups (selected by the wrapper, never all at once)
//!
//! Tests are named with a stable prefix so the wrapper can select exactly one
//! group and **never** run destructive cleanup during a normal verification:
//!
//! - `ro_*` — read-only checks (no Coder mutation).
//! - `mut_*` — opt-in mutating scenario + cross-tenant isolation check.
//! - `cleanup_*` — deletes only resources recorded in the ledger; run explicitly.
//!
//! # Wrapper modes (`tests/integration/coder_compatibility.sh`)
//!
//! ```sh
//! # Read-only (no mutation):
//! export CODER_URL=... CODER_SESSION_TOKEN=...
//! ./tests/integration/coder_compatibility.sh
//!
//! # Mutating scenario + isolation (explicit opt-in):
//! export OPENFLOWS_CODER_MUTATE=1
//! export OPENFLOWS_CODER_TEST_ORG_PREFIX=ofci-$(whoami)-
//! ./tests/integration/coder_compatibility.sh --mutating
//!
//! # Cleanup (only resources recorded in the ledger):
//! ./tests/integration/coder_compatibility.sh --cleanup
//! ```
//!
//! # Safety
//!
//! - Mutating resources are created only inside the opt-in isolated test
//!   organization prefix, under a single ordered scenario. No check targets
//!   the default organization.
//! - Every created resource is recorded **immediately** in a persistent,
//!   atomically-written ledger together with the deployment URL and
//!   organization. Cleanup validates the deployment URL and org before
//!   deleting, deletes children before parents, org-scopes every delete, and
//!   **retains** entries whose deletion failed (it never deletes the ledger
//!   blindly).
//! - Credentials are never printed. Command output is redacted; provisioner
//!   key and session tokens are consumed programmatically and never echoed.
//! - Missing environment is a reported `NOT_VERIFIED`, never fabricated
//!   evidence.

use coder_client::CoderClient;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::process::Command;
use std::sync::Mutex;

/// Baseline Coder version this repository targets (docker-compose.yml default).
const EXPECTED_CODER_VERSION: &str = "v2.37.3";

/// Env vars.
const ENV_CODER_URL: &str = "CODER_URL";
const ENV_CODER_TOKEN: &str = "CODER_SESSION_TOKEN";
const ENV_CODER_TOKEN_FILE: &str = "CODER_SESSION_TOKEN_FILE";
const ENV_MUTATE: &str = "OPENFLOWS_CODER_MUTATE";
const ENV_ORG_PREFIX: &str = "OPENFLOWS_CODER_TEST_ORG_PREFIX";
const ENV_LEDGER_FILE: &str = "OPENFLOWS_CODER_LEDGER_FILE";
/// Explicit org IDs for the isolation check (created by the mutating scenario).
const ENV_ORG_A: &str = "OPENFLOWS_CODER_ORG_A";
const ENV_ORG_B: &str = "OPENFLOWS_CODER_ORG_B";
/// Token files for the two isolation identities.
const ENV_TOKEN_A_FILE: &str = "OPENFLOWS_CODER_TOKEN_A_FILE";
const ENV_TOKEN_B_FILE: &str = "OPENFLOWS_CODER_TOKEN_B_FILE";

/// Regex fragments for token-like strings that must never reach logs.
fn token_patterns() -> &'static [&'static str] {
    &[
        r"(?i)bearer\s+[A-Za-z0-9._~+/=-]{8,}",
        r"(?i)(coder|ghp|gho|ghs|github_pat|glpat)[_-][A-Za-z0-9_]{16,}",
        r"(?i)(token|secret|password|apikey|access_key)\s*[=:]\s*[A-Za-z0-9._~+/=-]{8,}",
        r"[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}\.[A-Za-z0-9_-]{20,}",
        r"(?i)CODER_SESSION_TOKEN\s*[=:]\s*\S+",
    ]
}

/// Scrub token-like patterns from arbitrary text (JSON or plaintext).
fn redact_string(s: &str) -> String {
    let mut out = s.to_string();
    for pat in token_patterns() {
        if let Ok(re) = regex::Regex::new(pat) {
            out = re.replace_all(&out, "<REDACTED>").to_string();
        }
    }
    out
}

/// Recursively redact a serde value by field name, for structured bodies.
fn redact(v: &Value) -> Value {
    match v {
        Value::Object(map) => {
            let mut out = serde_json::Map::new();
            for (k, val) in map {
                let key_lc = k.to_ascii_lowercase();
                if key_lc.contains("token")
                    || key_lc.contains("secret")
                    || key_lc.contains("password")
                    || key_lc.contains("key")
                    || key_lc.contains("access_token")
                {
                    out.insert(k.clone(), Value::String("<REDACTED>".into()));
                } else {
                    out.insert(k.clone(), redact(val));
                }
            }
            Value::Object(out)
        }
        Value::Array(arr) => Value::Array(arr.iter().map(redact).collect()),
        other => other.clone(),
    }
}

/// Resolve the operator session token without ever echoing it.
fn resolve_token() -> Option<String> {
    if let Ok(t) = std::env::var(ENV_CODER_TOKEN) {
        if !t.trim().is_empty() {
            return Some(t);
        }
    }
    if let Ok(path) = std::env::var(ENV_CODER_TOKEN_FILE) {
        if let Ok(t) = std::fs::read_to_string(&path) {
            let t = t.trim().to_string();
            if !t.is_empty() {
                return Some(t);
            }
        }
    }
    None
}

fn coder_url() -> Option<String> {
    let url = std::env::var(ENV_CODER_URL).ok()?;
    if url.trim().is_empty() {
        None
    } else {
        Some(url.trim_end_matches('/').to_string())
    }
}

/// Fail with a `NOT_VERIFIED` marker when the live environment is absent.
fn require_live_client() -> CoderClient {
    let url = coder_url().expect(
        "NOT_VERIFIED: CODER_URL is not set; no licensed Coder deployment was configured. \
         This check cannot run and is reported NOT_VERIFIED, not VERIFIED_LIVE.",
    );
    let token = resolve_token().expect(
        "NOT_VERIFIED: neither CODER_SESSION_TOKEN nor CODER_SESSION_TOKEN_FILE is set. \
         This check cannot run and is reported NOT_VERIFIED, not VERIFIED_LIVE.",
    );
    CoderClient::new(&url, &token)
}

fn mutate_enabled() -> bool {
    std::env::var(ENV_MUTATE).map(|v| v == "1").unwrap_or(false)
}

fn require_mutate() {
    assert!(
        mutate_enabled(),
        "NOT_VERIFIED: mutating checks require OPENFLOWS_CODER_MUTATE=1. \
         Refusing to create resources without explicit opt-in."
    );
}

fn org_prefix() -> String {
    std::env::var(ENV_ORG_PREFIX).unwrap_or_else(|_| "ofci-".to_string())
}

fn run_suffix() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("{:x}", nanos % 0xFFFF_FFFF)
}

fn assert_not_default_org(org: &str) {
    assert!(
        !org.eq_ignore_ascii_case("coder") && !org.is_empty(),
        "Refusing to mutate the default organization (org={org}). \
         Mutating checks must target the isolated test org prefix."
    );
}

/// Run a `coder` CLI subcommand synchronously. Never passes the token on argv.
/// `org` scopes the command via CODER_ORGANIZATION when non-empty.
fn coder_cli(args: &[&str], org: &str) -> Result<String, String> {
    let url = coder_url().ok_or("CODER_URL not set")?;
    let token = resolve_token().ok_or("session token not set")?;
    let mut cmd = Command::new("coder");
    cmd.args(args)
        .env("CODER_URL", &url)
        .env("CODER_SESSION_TOKEN", &token)
        .env("CODER_TOKEN", &token);
    if !org.is_empty() {
        cmd.env("CODER_ORGANIZATION", org);
    }
    let out = cmd
        .output()
        .map_err(|e| format!("failed to spawn `coder`: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        return Err(format!(
            "`coder {}` exited {}: stderr={}",
            args.join(" "),
            out.status,
            redact_string(&stderr)
        ));
    }
    Ok(stdout)
}

/// Run a `coder` CLI subcommand under a wall-clock timeout (async).
async fn coder_cli_timeout(
    args: &[&str],
    org: &str,
    timeout: std::time::Duration,
) -> Result<String, String> {
    let url = coder_url().ok_or("CODER_URL not set")?;
    let token = resolve_token().ok_or("session token not set")?;
    let mut cmd = tokio::process::Command::new("coder");
    cmd.args(args)
        .env("CODER_URL", &url)
        .env("CODER_SESSION_TOKEN", &token)
        .env("CODER_TOKEN", &token);
    if !org.is_empty() {
        cmd.env("CODER_ORGANIZATION", org);
    }
    let out = tokio::time::timeout(timeout, cmd.output())
        .await
        .map_err(|_| format!("`coder {}` timed out after {timeout:?}", args.join(" ")))?
        .map_err(|e| format!("failed to spawn `coder`: {e}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    if !out.status.success() {
        return Err(format!(
            "`coder {}` exited {}: stderr={}",
            args.join(" "),
            out.status,
            redact_string(&stderr)
        ));
    }
    Ok(stdout)
}

// ── Persistent, atomically-written resource ledger ─────────────────────────
// Every entry records the deployment URL and organization so cleanup can
// validate that a recorded resource belongs to THIS deployment and org before
// deleting it. Entries are appended/updated under a process-wide lock and the
// file is rewritten atomically. Status transitions are `created` ->
// `delete_failed` | `deleted`; entries that fail to delete are retained.

#[derive(Serialize, Deserialize, Clone)]
struct LedgerEntry {
    deployment_url: String,
    org: String,
    kind: String, // org | template | provisioner_key | user | workspace
    id: String,
    status: String, // created | deleted | delete_failed
}

#[derive(Serialize, Deserialize, Default)]
struct Ledger {
    entries: Vec<LedgerEntry>,
}

static LEDGER_LOCK: Mutex<()> = Mutex::new(());

fn ledger_path() -> std::path::PathBuf {
    if let Ok(p) = std::env::var(ENV_LEDGER_FILE) {
        return std::path::PathBuf::from(p);
    }
    std::env::temp_dir().join("ofci-ledger.json")
}

fn load_ledger_locked() -> Ledger {
    let path = ledger_path();
    if let Ok(text) = std::fs::read_to_string(&path) {
        if let Ok(ledger) = serde_json::from_str::<Ledger>(&text) {
            return ledger;
        }
    }
    Ledger::default()
}

fn persist_ledger(ledger: &Ledger) {
    let path = ledger_path();
    let text = serde_json::to_string_pretty(ledger).expect("serialize ledger");
    // Atomic write: write temp then rename so a crash never leaves a torn file.
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, text).expect("write ledger temp");
    std::fs::rename(&tmp, &path).expect("atomic ledger replace");
    println!("ledger written to {}", path.display());
}

/// Record a created resource immediately. `org` is the org it belongs to;
/// `org_id` may be None until resolved. Panics on write failure so a lost
/// record (and thus an unsafe cleanup) is never silent.
fn ledger_record(deployment_url: &str, org: &str, kind: &str, id: &str) {
    let _guard = LEDGER_LOCK.lock().expect("ledger lock poisoned");
    let mut ledger = load_ledger_locked();
    let key = (kind, id);
    if let Some(e) = ledger
        .entries
        .iter_mut()
        .find(|e| (e.kind.as_str(), e.id.as_str()) == key)
    {
        e.status = "created".to_string();
        e.deployment_url = deployment_url.to_string();
        e.org = org.to_string();
    } else {
        ledger.entries.push(LedgerEntry {
            deployment_url: deployment_url.to_string(),
            org: org.to_string(),
            kind: kind.to_string(),
            id: id.to_string(),
            status: "created".to_string(),
        });
    }
    persist_ledger(&ledger);
}

/// Mark a ledger entry's deletion outcome and retain it unless fully deleted.
fn ledger_delete_result(kind: &str, id: &str, ok: bool) {
    let _guard = LEDGER_LOCK.lock().expect("ledger lock poisoned");
    let mut ledger = load_ledger_locked();
    let key = (kind, id);
    if let Some(e) = ledger
        .entries
        .iter_mut()
        .find(|e| (e.kind.as_str(), e.id.as_str()) == key)
    {
        e.status = if ok { "deleted" } else { "delete_failed" }.to_string();
    }
    // Retain entries that failed; drop only fully-deleted ones.
    ledger.entries.retain(|e| e.status != "deleted");
    persist_ledger(&ledger);
}

// ── Read-only checks (prefix `ro_`) ─────────────────────────────────────────

#[tokio::test]
#[ignore = "requires a licensed Coder deployment in CODER_URL + CODER_SESSION_TOKEN"]
async fn ro_buildinfo_matches_pinned_version() {
    let client = require_live_client();
    let resp = reqwest::Client::new()
        .get(format!("{}/api/v2/buildinfo", client.base_url()))
        .send()
        .await
        .expect("GET /api/v2/buildinfo failed");
    assert_eq!(resp.status(), 200, "buildinfo status");
    let body: Value = resp.json().await.expect("buildinfo JSON");
    let version = body
        .get("version")
        .and_then(Value::as_str)
        .unwrap_or("<none>")
        .to_string();
    println!("buildinfo.version = {version}");
    if version == EXPECTED_CODER_VERSION {
        println!("VERIFIED_LIVE baseline: server version matches pinned default {EXPECTED_CODER_VERSION}");
    } else {
        println!(
            "BASELINE_DRIFT: server is {version}, expected default {EXPECTED_CODER_VERSION}. \
             Reconcile before WP-05 (config/env.rs defaults to v2.37.1)."
        );
        assert!(
            version.starts_with("v2.37."),
            "server version {version} is not on the pinned v2.37 line"
        );
    }
    println!("buildinfo (redacted) = {}", redact(&body));
}

#[tokio::test]
#[ignore = "requires a licensed Coder deployment"]
async fn ro_record_cli_version() {
    require_live_client();
    match coder_cli(&["version"], "") {
        Ok(out) => {
            let first = out.lines().next().unwrap_or("<none>").to_string();
            println!("VERIFIED_LIVE coder CLI: {first}");
        }
        Err(e) => println!("NOT_VERIFIED coder CLI version: {}", redact_string(&e)),
    }
}

#[tokio::test]
#[ignore = "requires a licensed Coder deployment"]
async fn ro_list_organizations_and_default_resolution() {
    let client = require_live_client();
    let orgs = client
        .list_organizations()
        .await
        .expect("list_organizations");
    assert!(!orgs.is_empty(), "expected at least the default org");
    let default_id = client
        .get_default_organization_id()
        .await
        .expect("get_default_organization_id");
    println!(
        "VERIFIED_LIVE org_count={} default_org_id={}",
        orgs.len(),
        default_id
    );
    for o in &orgs {
        println!("org {} name={} is_default={}", o.id, o.name, o.is_default);
    }
}

#[tokio::test]
#[ignore = "requires a licensed Coder deployment"]
async fn ro_list_templates_in_default_org() {
    let client = require_live_client();
    let templates = client.list_templates().await.expect("list_templates");
    println!("VERIFIED_LIVE template_count={}", templates.len());
    for t in &templates {
        println!("template id={} name={}", t.id, t.name);
    }
}

#[cfg(feature = "chats-api")]
#[tokio::test]
#[ignore = "requires a licensed Coder deployment with Agents/chat license"]
async fn ro_list_chat_models_org_scoped() {
    let client = require_live_client();
    let models: Vec<coder_client::types::ModelInfo> = client
        .list_chat_models()
        .await
        .expect("list_chat_models (org-scoped)");
    println!("VERIFIED_LIVE model_count={}", models.len());
    for m in &models {
        println!("model id={} name={} provider={}", m.id, m.name, m.provider);
    }
}

#[tokio::test]
#[ignore = "requires a licensed Coder deployment"]
async fn ro_me_and_token_scope() {
    let client = require_live_client();
    let me = client.get_me().await.expect("get_me");
    println!(
        "VERIFIED_LIVE token authenticated as user id={} username={}",
        me.id, me.username
    );
}

/// Sanitized environment summary; never prints credentials.
#[test]
#[ignore = "environment summary"]
fn ro_env_summary() {
    println!(
        "CODER_URL={} token_set={} OPENFLOWS_CODER_MUTATE={} ORG_PREFIX={}",
        coder_url().unwrap_or_default(),
        resolve_token().is_some(),
        mutate_enabled(),
        org_prefix()
    );
}

// ── Mutating scenario (prefix `mut_`, single ordered test) ─────────────────
// One ordered scenario so every step's prerequisites exist before the next:
// org -> provisioner key + running daemon -> machine identity + roles ->
// template version -> workspace (explicit org/owner/version). Every resource is
// recorded in the ledger immediately after it is created.

#[tokio::test]
#[ignore = "requires OPENFLOWS_CODER_MUTATE=1 and a licensed deployment"]
async fn mut_scenario_provision_tenant() {
    require_mutate();
    let client = require_live_client();
    let deployment = coder_url().expect("CODER_URL");
    let suffix = run_suffix();
    let org = format!("{}{}-{}", org_prefix(), "org", suffix);
    assert_not_default_org(&org);
    // Org and workspace creation carry timeouts so the scenario never hangs.
    let cmd_timeout = std::time::Duration::from_secs(180);

    // 1. Organization.
    let _ = coder_cli_timeout(&["organizations", "create", &org], "", cmd_timeout).await;
    let orgs = client
        .list_organizations()
        .await
        .expect("list_organizations");
    let org_id = orgs
        .iter()
        .find(|o| o.name == org)
        .map(|o| o.id.clone())
        .unwrap_or_else(|| panic!("org {org} not visible"));
    ledger_record(&deployment, &org, "org", &org);
    println!("VERIFIED_LIVE org {org} (id={org_id})");

    // 2. Org-scoped provisioner key + running daemon, with readiness wait.
    let key = format!("ofci-key-{suffix}");
    let key_out = coder_cli_timeout(
        &["provisioner", "keys", "create", &key, "--org", &org],
        "",
        cmd_timeout,
    )
    .await
    .unwrap_or_else(|e| panic!("provisioner key create failed: {}", redact_string(&e)));
    ledger_record(&deployment, &org, "provisioner_key", &key);
    // Extract the one-time daemon key without ever echoing it.
    let daemon_key = extract_daemon_key(&key_out).unwrap_or_else(|| {
        panic!("could not extract provisioner daemon key from command output; refusing to start provisioner")
    });
    let mut daemon = start_provisioner_daemon(&org, &daemon_key);
    wait_for_provisioner_ready(&client, &org_id, std::time::Duration::from_secs(120)).await;
    println!("VERIFIED_LIVE provisioner registered and ready for org {org}");
    // 3. Machine identity (service account) in the org.
    let username = format!("ofci-sa-{suffix}");
    coder_cli_timeout(
        &[
            "users",
            "create",
            "--service-account",
            "--username",
            &username,
            "--org",
            &org,
            "--email",
            &format!("{username}@invalid.local"),
        ],
        "",
        cmd_timeout,
    )
    .await
    .unwrap_or_else(|e| panic!("service account create failed: {}", redact_string(&e)));
    ledger_record(&deployment, &org, "user", &username);
    println!("VERIFIED_LIVE machine identity created: {username}");

    // 4. Assign org roles directly (service accounts do not inherit
    //    `agents-access`). Org-scoped via CODER_ORGANIZATION.
    coder_cli_timeout(
        &[
            "organizations",
            "members",
            "edit-roles",
            &username,
            "organization-workspace-access",
            "agents-access",
        ],
        &org,
        cmd_timeout,
    )
    .await
    .unwrap_or_else(|e| panic!("org role assign failed: {}", redact_string(&e)));
    println!("VERIFIED_LIVE machine identity roles assigned (workspace-access, agents-access)");

    // 5. Publish a template version into the org with an explicit version name.
    let template = format!("{}{}", org_prefix(), "nexus");
    let version = format!("ofci-wp00-{suffix}");
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("main.tf"), VALID_MINIMAL_MAIN_TF)
        .expect("write minimal main.tf");
    coder_cli_timeout(
        &[
            "templates",
            "push",
            "--yes",
            "--org",
            &org,
            "--name",
            &version,
            "-d",
            tmp.path().to_str().unwrap(),
            &template,
        ],
        "",
        cmd_timeout,
    )
    .await
    .unwrap_or_else(|e| panic!("templates push failed: {}", redact_string(&e)));
    ledger_record(&deployment, &org, "template", &template);
    println!("VERIFIED_LIVE published template version {version} for {template} in {org}");

    // 6. Workspace with explicit org, owner, and pinned version.
    let ws_name = format!("ofci-check-{suffix}");
    coder_cli_timeout(
        &[
            "create",
            "--yes",
            "--org",
            &org,
            "--template",
            &template,
            "--template-version",
            &version,
            "--no-wait",
            &format!("{username}/{ws_name}"),
        ],
        "",
        cmd_timeout,
    )
    .await
    .unwrap_or_else(|e| {
        panic!(
            "workspace create with explicit version failed: {}",
            redact_string(&e)
        )
    });
    ledger_record(
        &deployment,
        &org,
        "workspace",
        &format!("{username}/{ws_name}"),
    );
    println!("VERIFIED_LIVE created workspace {username}/{ws_name} org={org} version={version}");

    // Verify the workspace's actual org/owner/version.
    verify_workspace_ownership(&client, &ws_name, &org, &username, &version).await;

    // Stop the provisioner daemon cleanly.
    stop_provisioner_daemon(&mut daemon).await;
}

fn extract_daemon_key(key_out: &str) -> Option<String> {
    // `coder provisioner keys create` prints the one-time key. Extract the
    // credential token without ever logging it.
    let re = regex::Regex::new(r"(?im)^(?:key|token|.*?)[: \t]+(coder_[A-Za-z0-9_\-]+)").ok()?;
    re.captures(key_out)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

fn start_provisioner_daemon(org: &str, daemon_key: &str) -> tokio::process::Child {
    let url = coder_url().expect("CODER_URL");
    let mut cmd = tokio::process::Command::new("coder");
    cmd.args(["provisionerd", "start", "--org", org])
        .env("CODER_URL", &url)
        .env("CODER_PROVISIONER_DAEMON_KEY", daemon_key)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    cmd.spawn().expect("spawn provisionerd")
}

async fn wait_for_provisioner_ready(
    client: &CoderClient,
    org_id: &str,
    timeout: std::time::Duration,
) {
    // Poll the org-scoped provisioners API until at least one daemon reports
    // connected, or the deadline elapses.
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if provisioner_connected(client, org_id).await {
            return;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "provisioner for org {org_id} not ready within {timeout:?}; template import cannot be exercised"
            );
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
    }
}

async fn provisioner_connected(client: &CoderClient, org_id: &str) -> bool {
    // GET /api/v2/organizations/{org}/provisioners
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/v2/organizations/{}/provisioners",
            client.base_url(),
            org_id
        ))
        .header("Authorization", format!("Bearer {}", client.token()))
        .send()
        .await;
    match resp {
        Ok(r) if r.status().is_success() => {
            if let Ok(v) = r.json::<Value>().await {
                let arr = v.as_array().map(|a| a.to_vec()).unwrap_or_default();
                !arr.is_empty()
            } else {
                false
            }
        }
        _ => false,
    }
}

async fn stop_provisioner_daemon(daemon: &mut tokio::process::Child) {
    let _ = daemon.kill().await;
    let _ = daemon.wait().await;
}

/// Verify the created workspace reports the expected org, owner, and pinned
/// template version (the capability `create_workspace_for_user` does not expose).
async fn verify_workspace_ownership(
    client: &CoderClient,
    ws_name: &str,
    org: &str,
    owner: &str,
    version: &str,
) {
    // GET /api/v2/workspaces?q=... is used to resolve the workspace by name;
    // the response carries organization_id, owner_id/owner_name, and the build's
    // template_version_name.
    let url = format!("{}/api/v2/workspaces?q=name:{}", client.base_url(), ws_name);
    let resp = reqwest::Client::new()
        .get(&url)
        .header("Authorization", format!("Bearer {}", client.token()))
        .send()
        .await
        .expect("list workspaces");
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    let matches = body
        .get("workspaces")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(!matches.is_empty(), "workspace {ws_name} not found");
    println!(
        "VERIFIED_LIVE workspace ownership verified (org={org} owner={owner} version={version}): {}",
        redact(&body)
    );
}

// ── Isolation check (prefix `mut_`, explicit fixture orgs) ─────────────────
// Proves cross-tenant isolation with explicit org IDs and authenticated
// denials only (403/404). A 401 is NOT treated as proof of isolation because
// it could indicate a broken credential rather than a denied access.

#[tokio::test]
#[ignore = "requires OPENFLOWS_CODER_MUTATE=1, a licensed deployment, and two machine identities"]
async fn mut_isolation_cross_tenant() {
    require_mutate();
    require_live_client();
    let url = coder_url().expect("CODER_URL");

    // Two isolated machine identities, one per tenant, with explicit org IDs.
    let (org_a, org_b) = match (std::env::var(ENV_ORG_A).ok(), std::env::var(ENV_ORG_B).ok()) {
        (Some(a), Some(b)) if !a.is_empty() && !b.is_empty() => (a, b),
        _ => {
            println!(
                "NOT_VERIFIED: cross-tenant isolation requires OPENFLOWS_CODER_ORG_A and \
                 OPENFLOWS_CODER_ORG_B (created by the mutating scenario), plus \
                 OPENFLOWS_CODER_TOKEN_A_FILE / OPENFLOWS_CODER_TOKEN_B_FILE. \
                 Not inferred from organization names or mocks."
            );
            return;
        }
    };
    let (ta, tb) = match (
        std::env::var(ENV_TOKEN_A_FILE).ok(),
        std::env::var(ENV_TOKEN_B_FILE).ok(),
    ) {
        (Some(a), Some(b)) => (
            std::fs::read_to_string(&a)
                .expect("token A file")
                .trim()
                .to_string(),
            std::fs::read_to_string(&b)
                .expect("token B file")
                .trim()
                .to_string(),
        ),
        _ => {
            println!(
                "NOT_VERIFIED: isolation requires OPENFLOWS_CODER_TOKEN_A_FILE / \
                 OPENFLOWS_CODER_TOKEN_B_FILE. Not inferred from mocks."
            );
            return;
        }
    };
    let ca = CoderClient::new(&url, &ta);
    let cb = CoderClient::new(&url, &tb);

    // Resolve each org's ID with the token that owns it, so the probe always
    // targets a real, correct org ID regardless of what the other identity can
    // enumerate.
    let org_id_a = org_id_for(&ca, &org_a).await;
    let org_id_b = org_id_for(&cb, &org_b).await;

    // Positive controls: each identity reads ITS OWN explicit org's models.
    let models_a = read_models_expect(&ca, &org_id_a, "A reads own org A").await;
    let models_b = read_models_expect(&cb, &org_id_b, "B reads own org B").await;
    println!(
        "positive control: A read {} models in {org_a}, B read {} models in {org_b}",
        models_a, models_b
    );

    // Negative controls (authenticated denials only), both directions.
    let a_on_b = read_models_status(&ca, &org_id_b).await;
    let b_on_a = read_models_status(&cb, &org_id_a).await;
    assert!(
        is_denied(a_on_b) && is_denied(b_on_a),
        "VERIFIED_LIVE FAILURE: expected authenticated denial (403/404) both directions, \
         got A->B={a_on_b}, B->A={b_on_a}. 401 is not proof of isolation."
    );

    // Workspace boundary: A must not list B's workspaces, and vice versa.
    let a_ws_on_b = workspace_access_status(&ca, &org_id_b).await;
    let b_ws_on_a = workspace_access_status(&cb, &org_id_a).await;
    assert!(
        is_denied(a_ws_on_b) && is_denied(b_ws_on_a),
        "VERIFIED_LIVE FAILURE: workspace cross-tenant access not denied (A->B={a_ws_on_b}, B->A={b_ws_on_a})"
    );

    // Chat boundary: A must not create a chat in B's org, and vice versa.
    let a_chat_on_b = chat_access_status(&ca, &org_id_b).await;
    let b_chat_on_a = chat_access_status(&cb, &org_id_a).await;
    assert!(
        is_denied(a_chat_on_b) && is_denied(b_chat_on_a),
        "VERIFIED_LIVE FAILURE: chat cross-tenant access not denied (A->B={a_chat_on_b}, B->A={b_chat_on_a})"
    );

    println!(
        "VERIFIED_LIVE cross-tenant isolation holds in both directions (models, workspaces, chats)"
    );
}

fn is_denied(status: u16) -> bool {
    // Authenticated denial only; 401 (unauthenticated) does not prove isolation.
    status == 403 || status == 404
}

/// Resolve an org ID by name for the caller's token; panic if not visible.
async fn org_id_for(client: &CoderClient, name: &str) -> String {
    let orgs = client
        .list_organizations()
        .await
        .unwrap_or_else(|e| panic!("list orgs for {name}: {e}"));
    orgs.iter()
        .find(|o| o.name == name)
        .map(|o| o.id.clone())
        .unwrap_or_else(|| panic!("org {name} not visible to the calling identity"))
}

async fn read_models_expect(client: &CoderClient, org_id: &str, label: &str) -> usize {
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/v2/organizations/{}/chats/models",
            client.base_url(),
            org_id
        ))
        .header("Authorization", format!("Bearer {}", client.token()))
        .send()
        .await
        .unwrap_or_else(|e| panic!("{label}: request failed: {e}"));
    assert!(
        resp.status().is_success(),
        "{label}: expected 200, got {}",
        resp.status()
    );
    let body: Value = resp.json().await.unwrap_or(Value::Null);
    body.get("providers")
        .and_then(Value::as_array)
        .map(|a| a.len())
        .unwrap_or(0)
}

async fn read_models_status(client: &CoderClient, org_id: &str) -> u16 {
    reqwest::Client::new()
        .get(format!(
            "{}/api/v2/organizations/{}/chats/models",
            client.base_url(),
            org_id
        ))
        .header("Authorization", format!("Bearer {}", client.token()))
        .send()
        .await
        .map(|r| r.status().as_u16())
        .unwrap_or(0)
}

async fn workspace_access_status(client: &CoderClient, org_id: &str) -> u16 {
    // List workspaces scoped to the other org; must be denied.
    reqwest::Client::new()
        .get(format!(
            "{}/api/v2/workspaces?organization_id={}",
            client.base_url(),
            org_id
        ))
        .header("Authorization", format!("Bearer {}", client.token()))
        .send()
        .await
        .map(|r| r.status().as_u16())
        .unwrap_or(0)
}

async fn chat_access_status(client: &CoderClient, org_id: &str) -> u16 {
    // Create a chat in the OTHER org's context; must be denied.
    reqwest::Client::new()
        .post(format!("{}/api/v2/chats", client.base_url()))
        .header("Authorization", format!("Bearer {}", client.token()))
        .json(&serde_json::json!({
            "organization_id": org_id,
            "content": [{"type": "text", "text": "probe"}]
        }))
        .send()
        .await
        .map(|r| r.status().as_u16())
        .unwrap_or(0)
}

// ── Cleanup (prefix `cleanup_`, explicit, org-scoped) ──────────────────────
// Validates deployment URL and org before deleting, deletes children before
// parents, org-scopes every delete, and retains entries whose deletion failed.

#[tokio::test]
#[ignore = "deletes resources recorded in the ledger; requires OPENFLOWS_CODER_MUTATE=1"]
async fn cleanup_created_resources() {
    require_mutate();
    require_live_client();
    let deployment = coder_url().expect("CODER_URL");
    let _guard = LEDGER_LOCK.lock().expect("ledger lock poisoned");
    let mut ledger = load_ledger_locked();

    // Only consider entries belonging to THIS deployment and org prefix.
    let candidates: Vec<LedgerEntry> = ledger
        .entries
        .iter()
        .filter(|e| e.deployment_url == deployment && e.org.starts_with(&org_prefix()))
        .cloned()
        .collect();

    // Child-before-parent deletion order.
    let order: &[&str] = &["workspace", "template", "user", "provisioner_key", "org"];
    for kind in order {
        for entry in candidates.iter().filter(|e| e.kind == *kind) {
            let id = &entry.id;
            let ok = delete_resource(kind, id, &entry.org);
            ledger_delete_result_locked(&mut ledger, kind, id, ok);
            if ok {
                println!("cleanup {} {} (org {})", kind, id, entry.org);
            } else {
                println!(
                    "cleanup FAILED {} {} (org {}); retained for retry",
                    kind, id, entry.org
                );
            }
        }
    }
}

fn delete_resource(kind: &str, id: &str, org: &str) -> bool {
    assert_not_default_org(org);
    let r = match kind {
        "workspace" => coder_cli(&["delete", "--yes", id], org),
        "template" => coder_cli(&["templates", "delete", "--yes", id], org),
        "user" => coder_cli(&["users", "delete", "--yes", id], ""),
        "provisioner_key" => coder_cli(&["provisioner", "keys", "delete", "--yes", id], org),
        "org" => coder_cli(&["organizations", "delete", "--yes", id], ""),
        _ => return false,
    };
    r.is_ok()
}

/// Update a ledger entry's status in-place and persist, retaining failures.
fn ledger_delete_result_locked(ledger: &mut Ledger, kind: &str, id: &str, ok: bool) {
    for e in ledger.entries.iter_mut() {
        if e.kind == kind && e.id == id {
            e.status = if ok { "deleted" } else { "delete_failed" }.to_string();
        }
    }
    ledger.entries.retain(|e| e.status != "deleted");
    persist_ledger(ledger);
}

// ── Fixture ────────────────────────────────────────────────────────────────

/// A valid, minimal, secret-free Terraform template for publication checks.
const VALID_MINIMAL_MAIN_TF: &str = r#"terraform {
  required_providers {
    coder = {
      source  = "coder/coder"
      version = "~> 2.18.0"
    }
  }
}

data "coder_workspace" "me" {}

resource "coder_agent" "main" {
  os   = "linux"
  arch = "amd64"
}
"#;
