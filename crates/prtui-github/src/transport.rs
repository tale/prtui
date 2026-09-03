//! GitHub authentication and HTTP transport.

use super::{API_LIMIT, JSON_ACCEPT, Repo};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::process::Command;
use ureq::http::{HeaderMap, Response, StatusCode, Uri};
use ureq::{Agent, Body};

const USER_AGENT: &str = concat!("prtui/", env!("CARGO_PKG_VERSION"));
const API_TIMEOUT: Duration = Duration::from_secs(30);

/// GitHub sheds load with 429 and 5xx during an incident, and a connection it
/// drops mid-flight reads the same way. Two retries cover the seconds-long
/// blips without making a real outage feel like a hang.
const RETRY_LIMIT: u32 = 2;
const RETRY_BACKOFF: Duration = Duration::from_millis(400);
const RETRY_CEILING: Duration = Duration::from_secs(8);

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
pub async fn token(host: Option<&str>) -> Option<Arc<str>> {
    static TOKENS: OnceLock<Mutex<HashMap<String, Option<Arc<str>>>>> =
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

async fn ask_gh_for_token(host: &str) -> Option<Arc<str>> {
    let out = Command::new("gh")
        .args(["auth", "token", "--hostname", host])
        .output()
        .await
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!token.is_empty()).then(|| Arc::from(token))
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

pub fn check(response: &mut Response<Body>, what: &str) -> Result<()> {
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
///
/// Pagination is followed manually, so the agent's redirect policy cannot keep
/// the authorization header on its original host for us. A server-provided
/// cursor must therefore stay on the API origin that received the first page.
pub fn next_page(headers: &HeaderMap, origin: &Uri) -> Result<Option<String>> {
    let Some(link) = headers.get("link") else {
        return Ok(None);
    };
    let link = link.to_str().context("invalid pagination link header")?;

    let next = link
        .split(',')
        .filter(|part| part.contains("rel=\"next\""))
        .find_map(|part| {
            let start = part.find('<')? + 1;
            let end = part[start..].find('>')? + start;
            Some(part[start..end].to_string())
        });
    let Some(next) = next else {
        return Ok(None);
    };

    let cursor: Uri = next.parse().context("invalid pagination URL")?;
    if cursor.scheme() != origin.scheme()
        || cursor.authority() != origin.authority()
    {
        bail!("refusing a cross-origin pagination URL");
    }

    Ok(Some(next))
}

/// Which failures a request may be sent through again.
///
/// A read is safe to repeat however it failed. A write is not: a timeout or a
/// 502 says the answer went missing, never that the review was not filed, and
/// repeating one posts it twice. Only a refusal proves the request never ran.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Retry {
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

pub fn get(
    url: &str,
    accept: &str,
    token: Option<&str>,
) -> Result<Response<Body>> {
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

pub fn post(
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
pub fn graphql(
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
pub async fn write_token(repo: &Repo) -> Result<Arc<str>> {
    token(repo.host.as_deref()).await.with_context(|| {
        let host = repo.host.as_deref().unwrap_or("github.com");
        format!(
            "no GitHub token for {host}; run `gh auth login --hostname {host}`"
        )
    })
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
        let origin: Uri = "https://api.github.com/first".parse().unwrap();
        let middle = headers(
            "<https://api.github.com/repos/o/r/pulls/1/files?page=1>; rel=\"prev\", \
             <https://api.github.com/repos/o/r/pulls/1/files?page=3>; rel=\"next\", \
             <https://api.github.com/repos/o/r/pulls/1/files?page=9>; rel=\"last\"",
        );
        assert_eq!(
            next_page(&middle, &origin).unwrap().as_deref(),
            Some("https://api.github.com/repos/o/r/pulls/1/files?page=3")
        );

        let last = headers(
            "<https://api.github.com/repos/o/r/pulls/1/files?page=8>; rel=\"prev\", \
             <https://api.github.com/repos/o/r/pulls/1/files?page=1>; rel=\"first\"",
        );
        assert_eq!(next_page(&last, &origin).unwrap(), None);
        assert_eq!(next_page(&HeaderMap::new(), &origin).unwrap(), None);
    }

    #[test]
    fn pagination_cannot_carry_a_token_to_another_origin() {
        let origin: Uri = "https://api.github.com/first".parse().unwrap();

        for cursor in [
            "http://api.github.com/page/2",
            "https://github.example.com/page/2",
        ] {
            let links = headers(&format!("<{cursor}>; rel=\"next\""));
            let error = next_page(&links, &origin).unwrap_err().to_string();

            assert!(error.contains("cross-origin"), "{error}");
        }
    }

    /// A write that timed out may already have been filed. Sending it again
    /// posts the review twice, which is worse than reporting the timeout.
    #[test]
    fn only_a_refusal_lets_a_write_go_out_again() {
        let timeout = ureq::Error::Timeout(ureq::Timeout::Global);
        assert!(is_retryable_transport(Retry::Transient, &timeout));
        assert!(!is_retryable_transport(Retry::Refusals, &timeout));

        let refused = ureq::Error::ConnectionFailed;
        assert!(is_retryable_transport(Retry::Refusals, &refused));

        for retry in [Retry::Transient, Retry::Refusals] {
            assert!(is_retryable_status(retry, StatusCode::TOO_MANY_REQUESTS));
            assert!(!is_retryable_status(
                retry,
                StatusCode::UNPROCESSABLE_ENTITY
            ));
        }

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

        assert_eq!(failure_detail(br#"{"message":"Not Found"}"#), "Not Found");
        assert_eq!(failure_detail(b"  gateway timeout  "), "gateway timeout");
    }
}
