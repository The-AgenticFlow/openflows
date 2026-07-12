// binary/src/doctor.rs
//! Diagnostic checks shared by `openflows doctor` and `openflows-doctor`.

use anyhow::Result;

pub async fn run_checks() -> Result<()> {
    let mut all_pass = true;

    println!("openflows-doctor — Coder integration health check");
    println!();

    // 1. Coder server reachable
    let coder_url = std::env::var("CODER_URL");
    match &coder_url {
        Ok(url) => {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()?;
            match client
                .get(format!("{}/api/v2/buildinfo", url.trim_end_matches('/')))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    println!("  ✓ Coder server reachable at {}", url);
                }
                Ok(resp) => {
                    println!("  ✗ Coder server returned HTTP {} at {}", resp.status(), url);
                    println!("    Fix: Ensure Coder is running (docker compose up -d)");
                    all_pass = false;
                }
                Err(e) => {
                    println!("  ✗ Coder server not reachable at {}: {}", url, e);
                    println!("    Fix: Start Coder (docker compose up -d) and set CODER_URL");
                    all_pass = false;
                }
            }
        }
        Err(_) => {
            println!("  ✗ CODER_URL is not set");
            println!("    Fix: Set CODER_URL in .env (e.g., http://localhost:7080)");
            all_pass = false;
        }
    }

    // 2. Coder image tag
    let tag = std::env::var("CODER_IMAGE_TAG").unwrap_or_else(|_| "latest".to_string());
    println!("  ℹ Coder image tag: {} (pin for production)", tag);

    // 3. LLM provider/model configured
    if let Ok(url) = &coder_url {
        let token = std::env::var("CODER_SESSION_TOKEN")
            .or_else(|_| std::env::var("CODER_API_TOKEN"))
            .unwrap_or_default();
        if !token.is_empty() {
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()?;
            let resp = client
                .get(format!("{}/api/experimental/chats/models", url.trim_end_matches('/')))
                .header("Coder-Session-Token", &token)
                .send()
                .await;
            match resp {
                Ok(r) if r.status().is_success() => {
                    let body = r.text().await.unwrap_or_default();
                    if body.contains("\"id\"") {
                        println!("  ✓ LLM models configured in Coder");
                    } else {
                        println!("  ⚠ Could not verify LLM models — check Coder dashboard → AI Settings");
                    }
                }
                Ok(r) => {
                    println!("  ⚠ Chats API returned {} — ensure AI Agents are enabled", r.status());
                    println!("    Fix: Go to Coder dashboard → AI Settings → Coder Agents → Models");
                }
                Err(_) => {
                    println!("  ⚠ Could not reach Chats API — Coder may not support it yet");
                    println!("    Fix: Update Coder to a version with Coder Agents support");
                }
            }
        } else {
            println!("  ⚠ No Coder token set — cannot verify LLM config");
            println!("    Fix: Run openflows bootstrap first, or set CODER_SESSION_TOKEN");
        }
    }

    // 4. GitHub external auth configured
    let has_github_auth = std::env::var("CODER_EXTERNAL_AUTH_0_ID").is_ok()
        && std::env::var("CODER_EXTERNAL_AUTH_0_SECRET").is_ok();
    if has_github_auth {
        println!("  ✓ GitHub external auth configured (CODER_EXTERNAL_AUTH_0_ID/SECRET)");
    } else {
        println!("  ✗ GitHub external auth not configured");
        println!("    Fix: Create a GitHub OAuth App and set CODER_EXTERNAL_AUTH_0_ID");
        println!("         and CODER_EXTERNAL_AUTH_0_SECRET in .env, then restart Coder");
        all_pass = false;
    }

    // 5. Redis reachable
    match std::env::var("REDIS_URL") {
        Ok(url) => {
            match pocketflow_core::SharedStore::new_redis(&url).await {
                Ok(_) => println!("  ✓ Redis SharedStore reachable at {}", url),
                Err(e) => {
                    println!("  ✗ Redis not reachable at {}: {}", url, e);
                    println!("    Fix: Start Redis (docker compose up -d redis)");
                    all_pass = false;
                }
            }
        }
        Err(_) => {
            println!("  ✗ REDIS_URL is not set");
            println!("    Fix: Set REDIS_URL in .env (e.g., redis://localhost:6379)");
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
