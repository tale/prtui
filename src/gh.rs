use crate::text::url::escape_path;
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::process::Command;
use ureq::http::{HeaderMap, Response, StatusCode};
use ureq::{Agent, Body};

const THREADS_QUERY: &str = r"
query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      id number title state isDraft additions deletions changedFiles
      author { login }
      baseRefName headRefName headRefOid body
      files(first:100) { nodes { path viewerViewedState } }
      pendingReview: reviews(first:1, states:[PENDING]) { nodes { id } }
      discussion: comments(first:100) {
        nodes { id fullDatabaseId author { login } body createdAt }
      }
      reviewThreads(first:100) {
        nodes {
          id isResolved isOutdated viewerCanResolve path subjectType
          line originalLine diffSide startLine startDiffSide
          comments(first:50) {
            nodes { id fullDatabaseId state author { login } body createdAt }
          }
        }
      }
    }
  }
}
";

/// Opens the pending review as a side effect when the pull request has none,
/// which is how a first draft creates one without a round trip of its own.
const ADD_THREAD_MUTATION: &str = r"
mutation($input:AddPullRequestReviewThreadInput!) {
  addPullRequestReviewThread(input:$input) {
    thread {
      id
      comments(first:1) { nodes { id pullRequestReview { id } } }
    }
  }
}
";

const UPDATE_COMMENT_MUTATION: &str = r"
mutation($id:ID!, $body:String!) {
  updatePullRequestReviewComment(input:{pullRequestReviewCommentId:$id, body:$body}) {
    pullRequestReviewComment { id }
  }
}
";

const DELETE_COMMENT_MUTATION: &str = r"
mutation($id:ID!) {
  deletePullRequestReviewComment(input:{id:$id}) {
    pullRequestReviewComment { id }
  }
}
";

const SUBMIT_REVIEW_MUTATION: &str = r"
mutation($input:SubmitPullRequestReviewInput!) {
  submitPullRequestReview(input:$input) { pullRequestReview { id state } }
}
";

/// A verdict with no drafts under it has no pending review to publish, so it
/// files and submits one in a single call.
const CREATE_REVIEW_MUTATION: &str = r"
mutation($input:AddPullRequestReviewInput!) {
  addPullRequestReview(input:$input) { pullRequestReview { id state } }
}
";

const RESOLVE_MUTATION: &str = r"
mutation($id:ID!) {
  resolveReviewThread(input:{threadId:$id}) { thread { id isResolved } }
}
";

const UNRESOLVE_MUTATION: &str = r"
mutation($id:ID!) {
  unresolveReviewThread(input:{threadId:$id}) { thread { id isResolved } }
}
";

const MARK_VIEWED_MUTATION: &str = r"
mutation($id:ID!, $path:String!) {
  markFileAsViewed(input:{pullRequestId:$id, path:$path}) { clientMutationId }
}
";

const UNMARK_VIEWED_MUTATION: &str = r"
mutation($id:ID!, $path:String!) {
  unmarkFileAsViewed(input:{pullRequestId:$id, path:$path}) { clientMutationId }
}
";

const USER_AGENT: &str = concat!("prtui/", env!("CARGO_PKG_VERSION"));

const API_LIMIT: u64 = 64 * 1024 * 1024;
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// GitHub sheds load with 429 and 5xx during an incident, and a connection it
/// drops mid-flight reads the same way. Two retries cover the seconds-long
/// blips without making a real outage feel like a hang.
const RETRY_LIMIT: u32 = 2;
const RETRY_BACKOFF: Duration = Duration::from_millis(400);
const RETRY_CEILING: Duration = Duration::from_secs(8);

const JSON_ACCEPT: &str = "application/vnd.github+json";

/// Serves a blob as itself rather than as base64 inside a JSON envelope.
const RAW_ACCEPT: &str = "application/vnd.github.raw";

/// A file big enough to exceed this is not one anybody expands into a terminal
/// pane, and holding it would cost more than the diff it decorates.
const BLOB_LIMIT: u64 = 8 * 1024 * 1024;

const STATUS_URL: &str = "https://www.githubstatus.com/api/v2/components.json";

/// The two components a review rides on. A degraded Actions or Pages says
/// nothing about why a diff would not load, and naming it would only mislead.
const STATUS_COMPONENTS: [&str; 2] = ["API Requests", "Pull Requests"];

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

    /// Reads `HOST/OWNER/REPO` out of a repository's web URL, which is how
    /// `gh` reports where a checkout actually points.
    pub fn from_url(url: &str) -> Result<Self> {
        let url = url.trim();
        let rest = url
            .strip_prefix("https://")
            .or_else(|| url.strip_prefix("http://"))
            .unwrap_or(url);

        Self::parse(rest.trim_end_matches(".git"))
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

    /// Where the repository is read by a person rather than by the API, which
    /// is what a permalink names and what the browser is handed.
    pub fn web_url(&self) -> String {
        let host = self.host.as_deref().unwrap_or("github.com");

        format!("https://{host}/{}/{}", self.owner, self.name)
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
/// The credential for one host.
///
/// `gh` holds a token per host and they are not interchangeable: the github.com
/// one is a bearer credential that an enterprise host has no business seeing,
/// and `-R HOST/OWNER/REPO` takes any host at all. Asking without naming the
/// host is what used to send it to whichever one the flag pointed at.
async fn token(host: Option<&str>) -> Option<String> {
    static TOKENS: OnceLock<Mutex<HashMap<String, Option<String>>>> =
        OnceLock::new();

    let cache = TOKENS.get_or_init(Mutex::default);
    let key = host.unwrap_or("github.com").to_owned();

    if let Some(hit) = cache.lock().ok()?.get(&key) {
        return hit.clone();
    }

    let fetched = ask_gh_for_token(&key).await;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(key, fetched.clone());
    }

    fetched
}

async fn ask_gh_for_token(host: &str) -> Option<String> {
    let out = Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .output()
        .await
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!token.is_empty()).then_some(token)
}

/// A validation failure carries a generic `message` and puts what actually went
/// wrong in `errors`, whose entries are either strings or objects naming the
/// offending field. Reporting only the message turns every one of them into
/// "Unprocessable Entity".
fn problems(val: &serde_json::Value) -> Vec<String> {
    val.get("errors")
        .and_then(|errors| errors.as_array())
        .map(|errors| {
            errors
                .iter()
                .map(|error| match error {
                    serde_json::Value::String(text) => text.clone(),
                    object => {
                        let detail = object
                            .get("message")
                            .and_then(|m| m.as_str())
                            .map_or_else(|| object.to_string(), str::to_string);

                        match object.get("field").and_then(|f| f.as_str()) {
                            Some(field) => format!("{field}: {detail}"),
                            None => detail,
                        }
                    }
                })
                .collect()
        })
        .unwrap_or_default()
}

fn failure_detail(body: &[u8]) -> String {
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) else {
        return String::from_utf8_lossy(body).trim().to_string();
    };

    let message = val
        .get("message")
        .and_then(|m| m.as_str())
        .unwrap_or_default();
    let problems = problems(&val);

    if problems.is_empty() {
        return message.to_string();
    }

    format!("{message}: {}", problems.join("; "))
}

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

    bail!("{what} failed: HTTP {status}: {}", failure_detail(&body))
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

/// Which failures a request may be sent through again.
///
/// A read is safe to repeat however it failed. A write is not: a timeout or a
/// 502 says the answer went missing, never that the review was not filed, and
/// repeating one posts it twice. Only a refusal proves the request never ran.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Retry {
    Transient,
    Refusals,
}

fn is_retryable_status(retry: Retry, status: StatusCode) -> bool {
    match retry {
        Retry::Transient => {
            status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error()
        }
        Retry::Refusals => status == StatusCode::TOO_MANY_REQUESTS,
    }
}

const fn is_retryable_transport(retry: Retry, error: &ureq::Error) -> bool {
    match retry {
        Retry::Transient => matches!(
            error,
            ureq::Error::Io(_)
                | ureq::Error::Timeout(_)
                | ureq::Error::ConnectionFailed
        ),
        // The connection never opened, so nothing was ever sent down it.
        Retry::Refusals => matches!(error, ureq::Error::ConnectionFailed),
    }
}

/// `Retry-After` is what GitHub sends when it is throttling rather than
/// failing, so it beats guessing; everything else backs off exponentially.
fn backoff(response: Option<&Response<Body>>, attempt: u32) -> Duration {
    response
        .and_then(|response| response.headers().get("retry-after"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .map_or_else(|| RETRY_BACKOFF * 2u32.pow(attempt), Duration::from_secs)
        .min(RETRY_CEILING)
}

/// Retries what `retry` allows and hands back everything else — including a
/// 4xx — for the caller to report. Runs inside `spawn_blocking`, so sleeping
/// here parks a worker rather than the runtime.
fn send(
    retry: Retry,
    call: impl Fn() -> Result<Response<Body>, ureq::Error>,
) -> Result<Response<Body>> {
    for attempt in 0..RETRY_LIMIT {
        let delay = match call() {
            Ok(response) if !is_retryable_status(retry, response.status()) => {
                return Ok(response);
            }
            Ok(response) => backoff(Some(&response), attempt),
            Err(error) if !is_retryable_transport(retry, &error) => {
                return Err(error).context("request to GitHub failed");
            }
            Err(_) => backoff(None, attempt),
        };

        std::thread::sleep(delay);
    }

    call().context("request to GitHub failed")
}

fn get(url: &str, accept: &str, token: Option<&str>) -> Result<Response<Body>> {
    send(Retry::Transient, || {
        let mut request = agent()
            .get(url)
            .header("accept", accept)
            .header("x-github-api-version", "2022-11-28");

        if let Some(token) = token {
            request =
                request.header("authorization", format!("Bearer {token}"));
        }

        request.call()
    })
}

fn post(
    url: &str,
    token: Option<&str>,
    body: &serde_json::Value,
    retry: Retry,
) -> Result<Response<Body>> {
    send(retry, || {
        let mut request = agent()
            .post(url)
            .header("accept", JSON_ACCEPT)
            .header("x-github-api-version", "2022-11-28");

        if let Some(token) = token {
            request =
                request.header("authorization", format!("Bearer {token}"));
        }

        request.send_json(body)
    })
}

/// GraphQL answers 200 with an `errors` array, so a successful status alone
/// says nothing about whether the operation ran.
fn graphql(
    url: &str,
    token: Option<&str>,
    query: &str,
    variables: &serde_json::Value,
    what: &str,
    retry: Retry,
) -> Result<serde_json::Value> {
    let body = serde_json::json!({ "query": query, "variables": variables });
    let mut response = post(url, token, &body, retry)?;
    check(&mut response, what)?;

    let val: serde_json::Value = response
        .body_mut()
        .with_config()
        .limit(API_LIMIT)
        .read_json()
        .context("failed to parse graphql response")?;

    if let Some(errors) = val.get("errors") {
        bail!("{what} failed: {errors}");
    }

    Ok(val)
}

/// Anything that writes needs a credential; failing here beats a 401 that reads
/// like the review itself was rejected.
async fn write_token(repo: &Repo) -> Result<String> {
    token(repo.host.as_deref()).await.with_context(|| {
        let host = repo.host.as_deref().unwrap_or("github.com");
        format!(
            "no GitHub token for {host}; run `gh auth login --hostname {host}`"
        )
    })
}

/// Changed files with their unified-diff patches. Measured faster than the
/// `Accept: v3.diff` endpoint, and arrives pre-split per file.
pub async fn fetch_files(
    repo: &Repo,
    number: u32,
) -> Result<serde_json::Value> {
    let token = token(repo.host.as_deref()).await;
    let first = repo.rest_url(&format!(
        "/repos/{}/{}/pulls/{number}/files?per_page=100",
        repo.owner, repo.name
    ));

    tokio::task::spawn_blocking(move || {
        let mut pages = Vec::new();
        let mut next = Some(first);

        while let Some(url) = next {
            let mut response = get(&url, JSON_ACCEPT, token.as_deref())?;
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
    let token = token(repo.host.as_deref()).await;
    let url = repo.graphql_url();
    let variables = serde_json::json!({
        "owner": repo.owner,
        "repo": repo.name,
        "number": number,
    });

    tokio::task::spawn_blocking(move || {
        graphql(
            &url,
            token.as_deref(),
            THREADS_QUERY,
            &variables,
            "fetching pull request metadata",
            Retry::Transient,
        )
    })
    .await
    .context("metadata fetch panicked")?
}

/// One file at one commit, which is what fills a gap the patch left out.
///
/// The contents endpoint serves the blob directly under the raw media type, so
/// nothing has to be pulled back out of a JSON envelope on the way in.
pub async fn fetch_blob(
    repo: &Repo,
    path: &str,
    commit: &str,
) -> Result<String> {
    let token = token(repo.host.as_deref()).await;
    let url = repo.rest_url(&format!(
        "/repos/{}/{}/contents/{}?ref={commit}",
        repo.owner,
        repo.name,
        escape_path(path)
    ));

    tokio::task::spawn_blocking(move || {
        let mut response = get(&url, RAW_ACCEPT, token.as_deref())?;
        check(&mut response, "fetching file contents")?;

        response
            .body_mut()
            .with_config()
            .limit(BLOB_LIMIT)
            .read_to_string()
            .context("failed to read file contents")
    })
    .await
    .context("file fetch panicked")?
}

/// A mutation that is not safe to send twice. A timeout or a 502 says the
/// answer went missing, never that the write did not land, so only a refusal
/// is retried.
async fn mutate(
    repo: &Repo,
    query: &'static str,
    variables: serde_json::Value,
    what: &'static str,
) -> Result<serde_json::Value> {
    let token = write_token(repo).await?;
    let url = repo.graphql_url();

    tokio::task::spawn_blocking(move || {
        graphql(&url, Some(&token), query, &variables, what, Retry::Refusals)
    })
    .await
    .context("mutation panicked")?
}

/// Files one draft comment against the pending review, opening that review when
/// `input` names only the pull request. Answers with the payload the two new
/// node ids are read out of.
pub async fn add_thread(
    repo: &Repo,
    input: serde_json::Value,
) -> Result<serde_json::Value> {
    mutate(
        repo,
        ADD_THREAD_MUTATION,
        serde_json::json!({ "input": input }),
        "saving draft",
    )
    .await
}

pub async fn update_comment(
    repo: &Repo,
    comment: Arc<str>,
    body: String,
) -> Result<()> {
    mutate(
        repo,
        UPDATE_COMMENT_MUTATION,
        serde_json::json!({ "id": &*comment, "body": body }),
        "updating draft",
    )
    .await
    .map(|_| ())
}

pub async fn delete_comment(repo: &Repo, comment: Arc<str>) -> Result<()> {
    mutate(
        repo,
        DELETE_COMMENT_MUTATION,
        serde_json::json!({ "id": &*comment }),
        "discarding draft",
    )
    .await
    .map(|_| ())
}

/// An approval carries no summary, and GitHub reads an empty string as one
/// rather than as its absence, so the field goes out only when it has text.
fn verdict(
    mut input: serde_json::Value,
    event: &str,
    body: String,
) -> serde_json::Value {
    input["event"] = event.into();
    if !body.is_empty() {
        input["body"] = body.into();
    }

    serde_json::json!({ "input": input })
}

/// Publishes the pending review the drafts were filed against. Everything it
/// carries is already on GitHub, so this sends a verdict and a summary only.
pub async fn submit_review(
    repo: &Repo,
    review: Arc<str>,
    event: &str,
    body: String,
) -> Result<()> {
    let input = serde_json::json!({ "pullRequestReviewId": &*review });

    mutate(
        repo,
        SUBMIT_REVIEW_MUTATION,
        verdict(input, event, body),
        "submitting review",
    )
    .await
    .map(|_| ())
}

/// Files and publishes a review in one call, for a verdict that carries no
/// drafts and so has no pending review waiting for it.
pub async fn create_review(
    repo: &Repo,
    pull_request: Arc<str>,
    event: &str,
    body: String,
) -> Result<()> {
    let input = serde_json::json!({ "pullRequestId": &*pull_request });

    mutate(
        repo,
        CREATE_REVIEW_MUTATION,
        verdict(input, event, body),
        "submitting review",
    )
    .await
    .map(|_| ())
}

/// A reply is a standalone comment addressed to the thread's first comment; it
/// posts immediately rather than waiting for a review to be submitted.
pub async fn reply(
    repo: &Repo,
    number: u32,
    in_reply_to: u64,
    body: String,
) -> Result<()> {
    let token = write_token(repo).await?;
    let url = repo.rest_url(&format!(
        "/repos/{}/{}/pulls/{number}/comments",
        repo.owner, repo.name
    ));
    let payload =
        serde_json::json!({ "body": body, "in_reply_to": in_reply_to });

    tokio::task::spawn_blocking(move || {
        let mut response = post(&url, Some(&token), &payload, Retry::Refusals)?;
        check(&mut response, "posting reply")
    })
    .await
    .context("reply panicked")?
}

pub async fn set_resolved(
    repo: &Repo,
    thread_id: Arc<str>,
    is_resolved: bool,
) -> Result<()> {
    let (query, what) = if is_resolved {
        (RESOLVE_MUTATION, "resolving thread")
    } else {
        (UNRESOLVE_MUTATION, "unresolving thread")
    };

    mutate(repo, query, serde_json::json!({ "id": &*thread_id }), what)
        .await
        .map(|_| ())
}

/// A file's read-through mark, which GitHub keys on the pull request node
/// rather than on the changed file: a path is all it takes to name one.
pub async fn set_viewed(
    repo: &Repo,
    pr: Arc<str>,
    path: Arc<str>,
    is_viewed: bool,
) -> Result<()> {
    let (query, what) = if is_viewed {
        (MARK_VIEWED_MUTATION, "marking the file viewed")
    } else {
        (UNMARK_VIEWED_MUTATION, "marking the file unviewed")
    };

    mutate(
        repo,
        query,
        serde_json::json!({ "id": &*pr, "path": &*path }),
        what,
    )
    .await
    .map(|_| ())
}

/// Statuspage ranks a component from `operational` up to `major_outage`.
fn severity(status: &str) -> u8 {
    match status {
        "major_outage" => 3,
        "partial_outage" => 2,
        "degraded_performance" => 1,
        _ => 0,
    }
}

/// The worst state among the components a review needs. Which ones they are
/// does not change what the reader can do about it, so the line just names the
/// incident and gets out of the way.
fn summarize_outage(val: &serde_json::Value) -> Option<String> {
    let worst = val
        .get("components")?
        .as_array()?
        .iter()
        .filter_map(|component| {
            let name = component.get("name")?.as_str()?;
            let status = component.get("status")?.as_str()?;
            STATUS_COMPONENTS.contains(&name).then_some(status)
        })
        .filter(|status| severity(status) > 0)
        .max_by_key(|status| severity(status))?;

    Some(format!("github {}", worst.replace('_', " ")))
}

/// GitHub's incident feed.
///
/// Statuspage runs on its own infrastructure, so it stays up through the
/// outages it reports. Only consulted once a request has already failed, which
/// keeps the healthy path free of it.
pub async fn fetch_outage(repo: &Repo) -> Option<String> {
    // githubstatus.com describes github.com. An enterprise host being down is
    // not something it has ever heard of.
    if repo.enterprise_host().is_some() {
        return None;
    }

    tokio::task::spawn_blocking(|| {
        let mut response = get(STATUS_URL, JSON_ACCEPT, None).ok()?;
        check(&mut response, "reading GitHub status").ok()?;

        let val: serde_json::Value = response
            .body_mut()
            .with_config()
            .limit(API_LIMIT)
            .read_json()
            .ok()?;

        summarize_outage(&val)
    })
    .await
    .ok()?
}

/// Resolved from the local git remotes, which is the CLI's job rather than an
/// API call.
pub async fn current_repo() -> Result<Repo> {
    // The web URL rather than `nameWithOwner`, which names the repository but
    // not the host it lives on. Dropping the host sent an enterprise checkout
    // to github.com.
    let out = Command::new("gh")
        .args(["repo", "view", "--json", "url", "--jq", ".url"])
        .output()
        .await
        .context("failed to spawn gh; is it installed and on PATH?")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("gh repo view failed: {}", err.trim());
    }

    Repo::from_url(String::from_utf8_lossy(&out.stdout).trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(link: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("link", link.parse().unwrap());
        headers
    }

    /// `gh repo view` used to be asked for `nameWithOwner`, which named the
    /// repository but not the host, so an enterprise checkout resolved to
    /// github.com and took the github.com token with it.
    #[test]
    fn a_web_url_keeps_the_host_it_names() {
        let enterprise =
            Repo::from_url("https://github.example.com/team/service").unwrap();
        assert_eq!(enterprise.host.as_deref(), Some("github.example.com"));
        assert_eq!(enterprise.enterprise_host(), Some("github.example.com"));
        assert_eq!(
            enterprise.rest_url("/x"),
            "https://github.example.com/api/v3/x"
        );

        // The public host is named the same way but is not an enterprise one,
        // so it still resolves to the public API.
        let public = Repo::from_url("https://github.com/cli/cli").unwrap();
        assert_eq!(public.owner, "cli");
        assert_eq!(public.enterprise_host(), None);
        assert_eq!(public.rest_url("/x"), "https://api.github.com/x");

        assert_eq!(
            Repo::from_url("https://github.com/cli/cli.git")
                .unwrap()
                .name,
            "cli"
        );
    }

    /// The public token is not valid on another host, and handing it over would
    /// give that host a credential it has no business seeing.
    #[test]
    fn every_host_is_asked_for_its_own_token() {
        let public = Repo::parse("cli/cli").unwrap();
        let enterprise =
            Repo::parse("github.example.com/team/service").unwrap();

        assert_eq!(public.host.as_deref(), None);
        assert_eq!(enterprise.host.as_deref(), Some("github.example.com"));
        assert_eq!(
            enterprise.web_url(),
            "https://github.example.com/team/service"
        );
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

    /// A write that timed out may already have been filed. Sending it again
    /// posts the review twice, which is worse than reporting the timeout.
    #[test]
    fn only_a_refusal_lets_a_write_go_out_again() {
        let timeout = ureq::Error::Timeout(ureq::Timeout::Global);
        assert!(is_retryable_transport(Retry::Transient, &timeout));
        assert!(!is_retryable_transport(Retry::Refusals, &timeout));

        // Nothing ever went down a connection that never opened.
        let refused = ureq::Error::ConnectionFailed;
        assert!(is_retryable_transport(Retry::Refusals, &refused));

        for retry in [Retry::Transient, Retry::Refusals] {
            assert!(is_retryable_status(retry, StatusCode::TOO_MANY_REQUESTS));
            assert!(!is_retryable_status(
                retry,
                StatusCode::UNPROCESSABLE_ENTITY
            ));
        }

        // A 502 says the answer went missing, never that the write did not run.
        assert!(is_retryable_status(
            Retry::Transient,
            StatusCode::BAD_GATEWAY
        ));
        assert!(!is_retryable_status(
            Retry::Refusals,
            StatusCode::BAD_GATEWAY
        ));
    }

    #[test]
    fn a_validation_failure_reports_what_was_actually_wrong() {
        let structured = br#"{
            "message": "Unprocessable Entity",
            "errors": [
                {
                    "resource": "PullRequestReview",
                    "code": "custom",
                    "field": "body",
                    "message": "body can't be blank"
                },
                "line must be part of the diff"
            ]
        }"#;
        assert_eq!(
            failure_detail(structured),
            "Unprocessable Entity: body: body can't be blank; \
             line must be part of the diff"
        );

        // A plain message still reads as it always did.
        assert_eq!(failure_detail(br#"{"message":"Not Found"}"#), "Not Found");
        assert_eq!(failure_detail(b"  gateway timeout  "), "gateway timeout");
    }

    #[test]
    fn reports_only_the_components_a_review_depends_on() {
        let feed = |components: serde_json::Value| serde_json::json!({ "components": components });

        // The real 2026-08-17 shape: plenty broken, two of it ours.
        let outage = feed(serde_json::json!([
            { "name": "Git Operations", "status": "degraded_performance" },
            { "name": "API Requests", "status": "major_outage" },
            { "name": "Pull Requests", "status": "major_outage" },
            { "name": "Actions", "status": "major_outage" },
            { "name": "Packages", "status": "operational" },
        ]));
        assert_eq!(
            summarize_outage(&outage).as_deref(),
            Some("github major outage")
        );

        // The worst of the two leads, so a partial outage is not hidden
        // behind a component that is merely slow.
        let mixed = feed(serde_json::json!([
            { "name": "API Requests", "status": "degraded_performance" },
            { "name": "Pull Requests", "status": "partial_outage" },
        ]));
        assert_eq!(
            summarize_outage(&mixed).as_deref(),
            Some("github partial outage")
        );

        // Everything we ride on is up, so the failure was this request's own
        // and claiming an incident would send the reader the wrong way.
        let elsewhere = feed(serde_json::json!([
            { "name": "API Requests", "status": "operational" },
            { "name": "Actions", "status": "major_outage" },
        ]));
        assert_eq!(summarize_outage(&elsewhere), None);
        assert_eq!(summarize_outage(&serde_json::json!({})), None);
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
