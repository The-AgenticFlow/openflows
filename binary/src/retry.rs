//! Startup retries for steps the controller cannot run without.
//!
//! A hosted controller talks to the Manager before it can do anything useful.
//! These helpers keep the process alive with exponential backoff while that
//! dependency recovers, instead of exiting and waiting for a restart.

use std::time::Duration;

const INITIAL_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(30);

/// Retry `attempt` until it succeeds, logging each failure.
pub async fn until_ready<F, Fut>(what: &str, mut attempt: F)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let mut delay = INITIAL_DELAY;
    while let Err(error) = attempt().await {
        tracing::warn!(
            %error,
            %what,
            retry_delay_secs = delay.as_secs(),
            "Startup step failed; retrying"
        );
        tokio::time::sleep(delay).await;
        delay = (delay * 2).min(MAX_DELAY);
    }
}
