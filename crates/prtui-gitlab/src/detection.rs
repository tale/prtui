use std::time::Duration;

use crate::GitLab;

impl GitLab {
    /// Recognizes the public GitLab host without a network request.
    pub const fn recognizes_host(self, host: &str) -> bool {
        host.eq_ignore_ascii_case("gitlab.com")
    }

    /// Identifies a self-hosted instance without sending credentials.
    pub async fn probe_host(self, host: &str, timeout: Duration) -> bool {
        let url = format!("https://{host}/api/v4/version");
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
                .get("x-gitlab-meta")
                .is_some_and(|value| !value.is_empty())
        })
        .await
        .unwrap_or(false)
    }
}
