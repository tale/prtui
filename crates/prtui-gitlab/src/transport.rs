//! GitLab authentication and HTTP transport.

use super::{API_LIMIT, PAGE_SIZE, Repo, host_of};
use anyhow::{Context, Result, bail};
use serde::de::DeserializeOwned;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use tokio::process::Command;
use ureq::http::{HeaderMap, Response, StatusCode};
use ureq::{Agent, Body};

const USER_AGENT: &str = concat!("prtui/", env!("CARGO_PKG_VERSION"));
const API_TIMEOUT: Duration = Duration::from_secs(30);

const RETRY_LIMIT: u32 = 2;
const RETRY_BACKOFF: Duration = Duration::from_millis(400);
const RETRY_CEILING: Duration = Duration::from_secs(8);

fn agent() -> &'static Agent {
    static AGENT: OnceLock<Agent> = OnceLock::new();

    AGENT.get_or_init(|| {
        let config = Agent::config_builder()
            .user_agent(USER_AGENT)
            .timeout_global(Some(API_TIMEOUT))
            .max_redirects(0)
            .http_status_as_error(false)
            .build();

        Agent::new_with_config(config)
    })
}

pub async fn token(host: &str) -> Option<Arc<str>> {
    static TOKENS: OnceLock<Mutex<HashMap<String, Option<Arc<str>>>>> =
        OnceLock::new();

    let cache = TOKENS.get_or_init(Mutex::default);

    if let Some(hit) = cache.lock().ok()?.get(host) {
        return hit.clone();
    }

    let fetched = ask_glab_for_token(host).await;
    if let Ok(mut cache) = cache.lock() {
        cache.insert(host.to_owned(), fetched.clone());
    }

    fetched
}

async fn ask_glab_for_token(host: &str) -> Option<Arc<str>> {
    let out = Command::new("glab")
        .args(["config", "get", "token", "--host", host])
        .output()
        .await
        .ok()?;

    if !out.status.success() {
        return None;
    }

    let token = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!token.is_empty()).then(|| Arc::from(token))
}

pub async fn write_token(repo: &Repo) -> Result<Arc<str>> {
    let host = host_of(repo);

    token(host).await.with_context(|| {
        format!(
            "no GitLab token for {host}; run `glab auth login --hostname {host}`"
        )
    })
}

fn failure_detail(body: &[u8]) -> String {
    let Ok(val) = serde_json::from_slice::<serde_json::Value>(body) else {
        return String::from_utf8_lossy(body).trim().to_string();
    };

    if let Some(error) = val.get("error").and_then(|e| e.as_str()) {
        return error.to_string();
    }

    let Some(message) = val.get("message") else {
        return val.to_string();
    };

    if let Some(text) = message.as_str() {
        return text.to_string();
    }

    let Some(fields) = message.as_object() else {
        return message.to_string();
    };

    let problems: Vec<String> = fields
        .iter()
        .map(|(field, problems)| match problems.as_array() {
            Some(list) => {
                let joined: Vec<String> = list
                    .iter()
                    .map(|p| {
                        p.as_str().map_or_else(|| p.to_string(), str::to_string)
                    })
                    .collect();

                format!("{field}: {}", joined.join(", "))
            }
            None => format!("{field}: {problems}"),
        })
        .collect();

    problems.join("; ")
}

pub fn check(response: &mut Response<Body>, what: &str) -> Result<()> {
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }

    if status.is_redirection() {
        let target = response
            .headers()
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("elsewhere");

        bail!(
            "{what} failed: the host redirected to {target} instead of \
             answering. A proxy in front of GitLab is decoding the percent-\
             encoded `/` that identifies a project or file; disable URL \
             normalization for this host."
        );
    }

    let body = response
        .body_mut()
        .with_config()
        .limit(API_LIMIT)
        .read_to_vec()
        .unwrap_or_default();

    bail!("{what} failed: HTTP {status}: {}", failure_detail(&body))
}

pub fn next_page(headers: &HeaderMap) -> Result<Option<u32>> {
    let Some(value) = headers.get("x-next-page") else {
        return Ok(None);
    };
    let value = value.to_str()?.trim();
    if value.is_empty() {
        return Ok(None);
    }

    Ok(Some(
        value.parse().context("invalid GitLab pagination header")?,
    ))
}

pub fn read_all<T: DeserializeOwned>(
    url: &str,
    token: Option<&str>,
    what: &str,
) -> Result<Vec<T>> {
    let separator = if url.contains('?') { '&' } else { '?' };
    let mut items = Vec::new();
    let mut page = 1;

    loop {
        let mut response = get(
            &format!("{url}{separator}per_page={PAGE_SIZE}&page={page}"),
            token,
        )?;
        check(&mut response, what)?;
        let next = next_page(response.headers())?;
        let bytes = response
            .body_mut()
            .with_config()
            .limit(API_LIMIT)
            .read_to_vec()
            .with_context(|| format!("failed to read {what}"))?;
        items.extend(
            serde_json::from_slice::<Vec<T>>(&bytes)
                .with_context(|| format!("failed to parse {what}"))?,
        );
        let Some(next) = next else {
            return Ok(items);
        };
        if next <= page {
            bail!("GitLab pagination did not advance beyond page {page}");
        }
        page = next;
    }
}

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

fn backoff(response: Option<&Response<Body>>, attempt: u32) -> Duration {
    response
        .and_then(|response| response.headers().get("retry-after"))
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
        .map_or_else(|| RETRY_BACKOFF * 2u32.pow(attempt), Duration::from_secs)
        .min(RETRY_CEILING)
}

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
                return Err(error).context("request to GitLab failed");
            }
            Err(_) => backoff(None, attempt),
        };

        std::thread::sleep(delay);
    }

    call().context("request to GitLab failed")
}

pub fn get(url: &str, token: Option<&str>) -> Result<Response<Body>> {
    send(Retry::Transient, || {
        let mut request = agent().get(url).header("accept", "application/json");

        if let Some(token) = token {
            request = request.header("private-token", token);
        }

        request.call()
    })
}

pub fn post(
    url: &str,
    token: &str,
    body: &serde_json::Value,
    retry: Retry,
) -> Result<Response<Body>> {
    send(retry, || {
        agent()
            .post(url)
            .header("accept", "application/json")
            .header("private-token", token)
            .send_json(body)
    })
}

pub fn put(
    url: &str,
    token: &str,
    body: &serde_json::Value,
) -> Result<Response<Body>> {
    send(Retry::Refusals, || {
        agent()
            .put(url)
            .header("accept", "application/json")
            .header("private-token", token)
            .send_json(body)
    })
}

pub fn delete(url: &str, token: &str) -> Result<Response<Body>> {
    send(Retry::Refusals, || {
        agent()
            .delete(url)
            .header("accept", "application/json")
            .header("private-token", token)
            .call()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("x-next-page", value.parse().unwrap());
        headers
    }

    #[test]
    fn pagination_stops_at_the_last_page() {
        assert_eq!(next_page(&headers("3")).unwrap(), Some(3));
        assert_eq!(next_page(&headers("")).unwrap(), None);
        assert_eq!(next_page(&HeaderMap::new()).unwrap(), None);
    }

    #[test]
    fn only_a_refusal_lets_a_write_go_out_again() {
        let timeout = ureq::Error::Timeout(ureq::Timeout::Global);
        assert!(is_retryable_transport(Retry::Transient, &timeout));
        assert!(!is_retryable_transport(Retry::Refusals, &timeout));

        let refused = ureq::Error::ConnectionFailed;
        assert!(is_retryable_transport(Retry::Refusals, &refused));

        for retry in [Retry::Transient, Retry::Refusals] {
            assert!(is_retryable_status(retry, StatusCode::TOO_MANY_REQUESTS));
            assert!(!is_retryable_status(retry, StatusCode::BAD_REQUEST));
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
        assert_eq!(
            failure_detail(br#"{"message":"404 Project Not Found"}"#),
            "404 Project Not Found"
        );
        assert_eq!(
            failure_detail(br#"{"error":"insufficient_scope"}"#),
            "insufficient_scope"
        );
        assert_eq!(
            failure_detail(
                br#"{"message":{"base":["line_code is invalid","sha missing"]}}"#
            ),
            "base: line_code is invalid, sha missing"
        );
        assert_eq!(failure_detail(b"  bad gateway  "), "bad gateway");
    }
    fn server(
        responses: Vec<(&'static str, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{BufRead, BufReader, Write};
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for (headers, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut request = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" || line.is_empty() {
                        break;
                    }
                    request.push_str(&line);
                }
                requests.push(request);
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n{headers}\r\n{body}", body.len()).unwrap();
            }
            requests
        });

        (url, handle)
    }

    #[test]
    fn listings_read_beyond_one_hundred_results_on_the_original_origin() {
        let first =
            serde_json::to_string(&(0..100).collect::<Vec<_>>()).unwrap();
        let (url, server) = server(vec![
            (
                "X-Next-Page: 2\r\nLink: <http://elsewhere.invalid/>; rel=next\r\n",
                first,
            ),
            ("X-Next-Page: \r\n", "[100]".to_owned()),
        ]);
        let items: Vec<u32> = read_all(
            &format!("{url}/projects?search=repo"),
            Some("test-token"),
            "projects",
        )
        .unwrap();
        assert_eq!(items, (0..101).collect::<Vec<_>>());
        let requests = server.join().unwrap();
        assert!(
            requests[0]
                .starts_with("GET /projects?search=repo&per_page=100&page=1 ")
        );
        assert!(
            requests[1]
                .starts_with("GET /projects?search=repo&per_page=100&page=2 ")
        );
        assert!(requests.iter().all(|request| {
            request.to_lowercase().contains("private-token: test-token")
        }));
    }

    #[test]
    fn invalid_or_repeated_pagination_fails_instead_of_truncating_or_looping() {
        assert!(next_page(&headers("invalid")).is_err());
        let (url, server) =
            server(vec![("X-Next-Page: 1\r\n", "[]".to_owned())]);
        let error = read_all::<u32>(&url, None, "items").unwrap_err();
        assert!(error.to_string().contains("did not advance"));
        server.join().unwrap();
    }
}
