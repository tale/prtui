use anyhow::{Context, Result, bail};
use std::sync::OnceLock;
use std::time::Duration;
use tokio::process::Command;
use tokio::sync::OnceCell;
use ureq::http::{HeaderMap, Response};
use ureq::{Agent, Body};

const THREADS_QUERY: &str = r"
query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      number title state isDraft additions deletions changedFiles
      author { login }
      baseRefName headRefName body
      reviewThreads(first:100) {
        nodes {
          id isResolved isOutdated path line originalLine diffSide
          comments(first:50) { nodes { id author { login } body createdAt } }
        }
      }
      reviews(first:50) { nodes { author { login } state submittedAt } }
    }
  }
}
";

const USER_AGENT: &str = concat!("prtui/", env!("CARGO_PKG_VERSION"));

/// A comment attachment past this size is not worth stalling review for.
const ASSET_LIMIT: u64 = 26_214_400;
const ASSET_TIMEOUT: Duration = Duration::from_secs(20);
const API_LIMIT: u64 = 64 * 1024 * 1024;
const API_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone)]
pub struct Repo {
    pub host: Option<String>,
    pub owner: String,
    pub name: String,
}

impl Repo {
    /// Accepts `OWNER/REPO` or `HOST/OWNER/REPO`, matching `gh -R`.
    pub fn parse(slug: &str) -> Result<Self> {
        let parts: Vec<&str> =
            slug.trim().split('/').filter(|s| !s.is_empty()).collect();

        let (host, owner, name) = match parts.as_slice() {
            [owner, name] => (None, *owner, *name),
            [host, owner, name] => (Some(host.to_string()), *owner, *name),
            _ => bail!("expected [HOST/]OWNER/REPO, got {slug}"),
        };

        Ok(Self {
            host,
            owner: owner.to_string(),
            name: name.to_string(),
        })
    }

    /// Enterprise installations serve the same API under `/api/v3`, so an
    /// explicit `github.com` has to resolve to the public host instead.
    fn enterprise_host(&self) -> Option<&str> {
        self.host
            .as_deref()
            .filter(|host| !host.eq_ignore_ascii_case("github.com"))
    }

    fn rest_url(&self, path: &str) -> String {
        match self.enterprise_host() {
            Some(host) => format!("https://{host}/api/v3{path}"),
            None => format!("https://api.github.com{path}"),
        }
    }

    fn graphql_url(&self) -> String {
        match self.enterprise_host() {
            Some(host) => format!("https://{host}/api/graphql"),
            None => "https://api.github.com/graphql".to_string(),
        }
    }
}

/// One pooled agent for the whole session so the files, metadata and attachment
/// requests share connections instead of paying a TLS handshake apiece.
/// Redirects drop the `authorization` header by default, which is what keeps a
/// token from leaking to the storage host an attachment redirects to.
fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();

    AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_global(Some(API_TIMEOUT))
            .http_status_as_error(false)
            .build();

        Agent::new_with_config(config)
    })
}

/// The CLI owns credential storage — keychain on macOS, secret service on
/// Linux, plain config elsewhere — so ask it rather than reimplementing that.
async fn token() -> Option<String> {
    static TOKEN: OnceCell<Option<String>> = OnceCell::const_new();

    TOKEN
        .get_or_init(|| async {
            let out = Command::new("gh")
                .args(["auth", "token"])
                .output()
                .await
                .ok()?;

            if !out.status.success() {
                return None;
            }

            let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!token.is_empty()).then_some(token)
        })
        .await
        .clone()
}

/// GitHub reports failures as JSON with a `message`; surface that rather than
/// the bare status line.
fn check(response: &mut Response<Body>, what: &str) -> Result<()> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    let body = response
        .body_mut()
        .with_config()
        .limit(API_LIMIT)
        .read_to_vec()
        .unwrap_or_default();

    let detail = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|val| {
            val.get("message")
                .and_then(|m| m.as_str())
                .map(str::to_string)
        })
        .unwrap_or_else(|| String::from_utf8_lossy(&body).trim().to_string());

    bail!("{what} failed: HTTP {status}: {detail}")
}

/// `Link: <url>; rel="next", <url>; rel="last"` — the cursor for the next page.
fn next_page(headers: &HeaderMap) -> Option<String> {
    let link = headers.get("link")?.to_str().ok()?;

    link.split(',')
        .filter(|part| part.contains("rel=\"next\""))
        .find_map(|part| {
            let start = part.find('<')? + 1;
            let end = part[start..].find('>')? + start;
            Some(part[start..end].to_string())
        })
}

fn get(url: &str, token: Option<&str>) -> Result<Response<Body>> {
    let mut request = agent()
        .get(url)
        .header("accept", "application/vnd.github+json")
        .header("x-github-api-version", "2022-11-28");

    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }

    request.call().context("request to GitHub failed")
}

/// Changed files with their unified-diff patches. Measured faster than the
/// `Accept: v3.diff` endpoint, and arrives pre-split per file.
pub async fn fetch_files(
    repo: &Repo,
    number: u32,
) -> Result<serde_json::Value> {
    let token = token().await;
    let first = repo.rest_url(&format!(
        "/repos/{}/{}/pulls/{number}/files?per_page=100",
        repo.owner, repo.name
    ));

    tokio::task::spawn_blocking(move || {
        let mut pages = Vec::new();
        let mut next = Some(first);

        while let Some(url) = next {
            let mut response = get(&url, token.as_deref())?;
            check(&mut response, "fetching changed files")?;
            next = next_page(response.headers());

            let page: serde_json::Value = response
                .body_mut()
                .with_config()
                .limit(API_LIMIT)
                .read_json()
                .context("failed to parse /files response")?;

            pages.push(page);
        }

        Ok(serde_json::Value::Array(pages))
    })
    .await
    .context("changed-file fetch panicked")?
}

/// PR metadata plus review threads in a single GraphQL round trip.
pub async fn fetch_meta(repo: &Repo, number: u32) -> Result<serde_json::Value> {
    let token = token().await;
    let url = repo.graphql_url();
    let body = serde_json::json!({
        "query": THREADS_QUERY,
        "variables": {
            "owner": repo.owner,
            "repo": repo.name,
            "number": number,
        },
    });

    tokio::task::spawn_blocking(move || {
        let mut request = agent()
            .post(&url)
            .header("accept", "application/vnd.github+json");

        if let Some(token) = &token {
            request =
                request.header("authorization", format!("Bearer {token}"));
        }

        let mut response =
            request.send_json(&body).context("graphql request failed")?;
        check(&mut response, "fetching pull request metadata")?;

        let val: serde_json::Value = response
            .body_mut()
            .with_config()
            .limit(API_LIMIT)
            .read_json()
            .context("failed to parse graphql response")?;

        if let Some(errors) = val.get("errors") {
            bail!("graphql error: {errors}");
        }

        Ok(val)
    })
    .await
    .context("metadata fetch panicked")?
}

/// Download a comment attachment. Attachments on github.com itself are private
/// to the repository, so they carry the CLI's token; the agent drops that header
/// if the download redirects to a storage host.
pub async fn fetch_asset(url: &str) -> Result<Vec<u8>> {
    let authorized = url.starts_with("https://github.com/");
    let token = if authorized { token().await } else { None };
    let url = url.to_string();

    tokio::task::spawn_blocking(move || {
        let mut request = agent()
            .get(&url)
            .config()
            .timeout_global(Some(ASSET_TIMEOUT))
            .build();

        if let Some(token) = &token {
            request =
                request.header("authorization", format!("Bearer {token}"));
        }

        let mut response = request.call().context("downloading attachment")?;
        check(&mut response, "downloading attachment")?;

        response
            .body_mut()
            .with_config()
            .limit(ASSET_LIMIT)
            .read_to_vec()
            .context("attachment was too large or the download was cut short")
    })
    .await
    .context("attachment fetch panicked")?
}

/// Resolved from the local git remotes, which is the CLI's job rather than an
/// API call.
pub async fn current_repo() -> Result<Repo> {
    let out = Command::new("gh")
        .args([
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "--jq",
            ".nameWithOwner",
        ])
        .output()
        .await
        .context("failed to spawn gh; is it installed and on PATH?")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("gh repo view failed: {}", err.trim());
    }

    Repo::parse(String::from_utf8_lossy(&out.stdout).trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(link: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("link", link.parse().unwrap());
        headers
    }

    #[test]
    fn follows_only_the_next_pagination_cursor() {
        let middle = headers(
            "<https://api.github.com/repos/o/r/pulls/1/files?page=1>; rel=\"prev\", \
             <https://api.github.com/repos/o/r/pulls/1/files?page=3>; rel=\"next\", \
             <https://api.github.com/repos/o/r/pulls/1/files?page=9>; rel=\"last\"",
        );
        assert_eq!(
            next_page(&middle).as_deref(),
            Some("https://api.github.com/repos/o/r/pulls/1/files?page=3")
        );

        // The final page offers prev and first only, which ends the walk.
        let last = headers(
            "<https://api.github.com/repos/o/r/pulls/1/files?page=8>; rel=\"prev\", \
             <https://api.github.com/repos/o/r/pulls/1/files?page=1>; rel=\"first\"",
        );
        assert_eq!(next_page(&last), None);
        assert_eq!(next_page(&HeaderMap::new()), None);
    }

    #[test]
    fn routes_enterprise_hosts_to_their_own_api_mount() {
        let public = Repo::parse("owner/repo").unwrap();
        assert_eq!(
            public.rest_url("/repos/owner/repo/pulls"),
            "https://api.github.com/repos/owner/repo/pulls"
        );
        assert_eq!(public.graphql_url(), "https://api.github.com/graphql");

        // An explicit github.com is the public API, not an enterprise mount.
        let explicit = Repo::parse("github.com/owner/repo").unwrap();
        assert_eq!(explicit.graphql_url(), "https://api.github.com/graphql");

        let enterprise = Repo::parse("ghe.corp/owner/repo").unwrap();
        assert_eq!(
            enterprise.rest_url("/repos/owner/repo/pulls"),
            "https://ghe.corp/api/v3/repos/owner/repo/pulls"
        );
        assert_eq!(enterprise.graphql_url(), "https://ghe.corp/api/graphql");
    }
}
