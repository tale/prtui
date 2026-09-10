use std::time::Duration;

use crate::GitHub;

impl GitHub {
    /// Recognizes public GitHub hosts without a network request.
    pub const fn recognizes_host(self, host: &str) -> bool {
        host.eq_ignore_ascii_case("github.com")
    }

    /// Identifies an enterprise host without sending credentials or following redirects.
    pub async fn probe_host(self, host: &str, timeout: Duration) -> bool {
        let url = format!("https://{host}/api/v3/meta");
        tokio::task::spawn_blocking(move || {
            let config = ureq::Agent::config_builder()
                .timeout_global(Some(timeout))
                .max_redirects(0)
                .http_status_as_error(false)
                .build();
            let agent = ureq::Agent::new_with_config(config);
            let Ok(response) = agent.get(&url).call() else {
                return false;
            };

            response
                .headers()
                .get("x-github-enterprise-version")
                .is_some_and(|value| !value.is_empty())
        })
        .await
        .unwrap_or(false)
    }
}
