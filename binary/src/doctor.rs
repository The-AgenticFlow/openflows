// binary/src/doctor.rs
//! Diagnostic checks shared by `openflows doctor` and `openflows-doctor`.

use anyhow::Result;
use config::Envconfig;

pub async fn run_checks() -> Result<()> {
    let mut all_pass = true;

    println!("openflows-doctor — Coder integration health check");
    println!();

    let coder = config::CoderConfig::init_from_env()?;

    // 1. Coder server reachable
    let coder_url = coder.url.clone();
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    let pinned = coder.image_tag.clone();
    let semver = is_semver_tag(&pinned);
    let pinned = match &pinned[..] {
        _ if !semver => pinned,
        t if t.starts_with('v') => t.to_string(),
        t => format!("v{t}"),
    };
    match client
        .get(format!(
            "{}/api/v2/buildinfo",
            coder_url.trim_end_matches('/')
        ))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            let version = body
                .get("version")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            println!("  ✓ Coder server reachable at {}", coder_url);
            if semver && !version.is_empty() && version != pinned {
                println!(
                    "  ⚠ Coder reports {} but the pinned tag is {} — models/chat may drift",
                    version, pinned
                );
                println!(
                    "    Fix: Set CODER_IMAGE_TAG={} in docker-compose (or match the running image)",
                    pinned
                );
            } else if !semver {
                println!(
                    "  ℹ Running Coder {}",
                    if version.is_empty() {
                        "(unknown)"
                    } else {
                        version
                    }
                );
            }
        }
        Ok(resp) => {
            println!(
                "  ✗ Coder server returned HTTP {} at {}",
                resp.status(),
                coder_url
            );
            println!("    Fix: Ensure Coder is running (docker compose up -d)");
            all_pass = false;
        }
        Err(e) => {
            println!("  ✗ Coder server not reachable at {}: {}", coder_url, e);
            println!("    Fix: Start Coder (docker compose up -d) and set CODER_URL");
            all_pass = false;
        }
    }

    // 2. Coder image tag
    let tag = coder.image_tag.clone();
    println!("  ℹ Coder image tag: {} (pin for production)", tag);

    // 3. LLM provider/model configured
    {
        let token = coder
            .session_token
            .clone()
            .or(std::env::var("CODER_API_TOKEN").ok())
            .unwrap_or_default();
        if !token.is_empty() {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?;
            let base = coder_url.trim_end_matches('/');

            // Models are organization-scoped in Coder v2.37+: resolve the
            // default organization first, then query the org-scoped endpoint.
            let org_id = resolve_default_org_id(&client, base, &token).await;

            if let Some(org_id) = org_id {
                let resp = client
                    .get(format!(
                        "{}/api/v2/organizations/{}/chats/models",
                        base, org_id
                    ))
                    .header("Coder-Session-Token", &token)
                    .send()
                    .await;
                match resp {
                    Ok(r) if r.status().is_success() => {
                        let body = r.text().await.unwrap_or_default();
                        if body.contains("\"id\"") {
                            println!("  ✓ LLM models configured in Coder");
                        } else {
                            println!(
                                "  ⚠ Could not verify LLM models — check Coder dashboard → AI Settings"
                            );
                        }
                    }
                    Ok(r) => {
                        println!(
                            "  ⚠ Chats API returned {} — ensure AI Agents are enabled",
                            r.status()
                        );
                        println!(
                            "    Fix: Go to Coder dashboard → AI Settings → Coder Agents → Models"
                        );
                    }
                    Err(_) => {
                        println!("  ⚠ Could not reach Chats API — Coder may not support it yet");
                        println!("    Fix: Update Coder to a version with Coder Agents support");
                    }
                }
            } else {
                println!(
                    "  ⚠ Could not resolve Coder default organization — cannot verify LLM models"
                );
                println!("    Fix: Go to Coder dashboard → AI Settings → Coder Agents → Models");
            }
        } else {
            println!("  ⚠ No Coder token set — cannot verify LLM config");
            println!("    Fix: Run openflows bootstrap first, or set CODER_SESSION_TOKEN");
        }
    }

    // 4. Local Manager GitHub authorization.
    let manager_url = std::env::var("OPENFLOWS_PUBLIC_URL")
        .unwrap_or_else(|_| "http://localhost:3002".to_string());
    let status_url = format!("{}/api/v1/local/github", manager_url.trim_end_matches('/'));
    let manager_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()?;
    match manager_client.get(&status_url).send().await {
        Ok(response) if response.status().is_success() => {
            let status: serde_json::Value = response.json().await.unwrap_or_default();
            if status["authorized"] == true {
                println!("  ✓ Local Manager GitHub authorization ready");
            } else {
                println!("  ⚠ Connect GitHub in the local Manager at {manager_url}");
            }
        }
        _ => println!("  ⚠ Local Manager unavailable at {manager_url}"),
    }

    // 5. Redis reachable
    let redis_url = config::InfraConfig::init_from_env()?.effective_redis_url();
    match pocketflow_core::SharedStore::new_redis(&redis_url).await {
        Ok(_) => println!("  ✓ Redis SharedStore reachable at {}", redis_url),
        Err(e) => {
            println!("  ✗ Redis not reachable at {}: {}", redis_url, e);
            println!("    Fix: Start Redis (docker compose up -d redis)");
            all_pass = false;
        }
    }

    println!();
    if all_pass {
        println!("All checks passed ✓");
    } else {
        println!("Some checks failed ✗ — see fixes above");
        std::process::exit(1);
    }

    Ok(())
}

/// Fetch `GET /api/v2/organizations` and resolve the default organization id,
/// preferring the one flagged `is_default`, else falling back to the first.
async fn resolve_default_org_id(
    client: &reqwest::Client,
    base: &str,
    token: &str,
) -> Option<String> {
    let resp = client
        .get(format!("{}/api/v2/organizations", base))
        .header("Coder-Session-Token", token)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let body: serde_json::Value = resp.json().await.ok()?;
    let orgs = body.as_array()?;
    let pick = orgs
        .iter()
        .find(|o| o.get("is_default").and_then(serde_json::Value::as_bool) == Some(true))
        .or_else(|| orgs.first())?;
    pick.get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .filter(|s| !s.is_empty())
}

/// Return true when `tag` looks like a semantic version (e.g. `v2.37.0` or
/// `2.37.0`). Floating tags such as `latest` or branch names return false so
/// they are never coerced into an invalid `vlatest` and never compared exactly.
fn is_semver_tag(tag: &str) -> bool {
    let t = tag.strip_prefix('v').unwrap_or(tag);
    let bytes = t.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        i += 1;
    }
    i > 0 && i < bytes.len() && bytes[i] == b'.'
}
