use std::time::Duration;

use am_proto::LocalModelPolicy;

use crate::{AppCore, CoreError};

const CONNECTIVITY_TIMEOUT: Duration = Duration::from_secs(3);
const STABILIZATION_GAP: Duration = Duration::from_millis(500);
const PROBE_URLS: &[&str] = &["https://api.openai.com", "https://api.anthropic.com"];

impl AppCore {
    pub(crate) async fn cloud_connectivity_stable(
        &self,
        policy: &LocalModelPolicy,
    ) -> Result<bool, CoreError> {
        let successes = policy.stable_successes.max(1);
        let http = reqwest::Client::builder()
            .timeout(CONNECTIVITY_TIMEOUT)
            .redirect(reqwest::redirect::Policy::limited(2))
            .build()
            .map_err(|err| CoreError::Other(format!("connectivity probe setup failed: {err}")))?;

        for attempt in 0..successes {
            if !cloud_connectivity_once(&http).await {
                return Ok(false);
            }
            if attempt + 1 < successes {
                tokio::time::sleep(STABILIZATION_GAP).await;
            }
        }
        Ok(true)
    }
}

async fn cloud_connectivity_once(http: &reqwest::Client) -> bool {
    for url in PROBE_URLS {
        if http.head(*url).send().await.is_ok() {
            return true;
        }
        if http.get(*url).send().await.is_ok() {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_urls_cover_cloud_agents() {
        assert!(PROBE_URLS.iter().any(|url| url.contains("openai")));
        assert!(PROBE_URLS.iter().any(|url| url.contains("anthropic")));
    }
}
