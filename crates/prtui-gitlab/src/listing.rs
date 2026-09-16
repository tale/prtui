use anyhow::{Context, Result, bail};
use prtui_core::{
    Changes, PullRequestList, PullRequestListItem, PullRequestListScope,
    PullRequestTarget, Repo, ReviewStatus,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;

use crate::{DEFAULT_HOST, PAGE_SIZE, host_of, transport, wire::WireUser};

const REPOSITORY_QUERY: &str = r"
query($path: ID!, $first: Int!, $after: String) {
  scope: project(fullPath: $path) {
    mergeRequests(state: opened, sort: UPDATED_DESC, first: $first, after: $after) {
      ...Listing
    }
  }
}";

const USER_QUERY: &str = r"
query($first: Int!, $after: String) {
  scope: currentUser {
    mergeRequests: authoredMergeRequests(state: opened, sort: UPDATED_DESC, first: $first, after: $after) {
      ...Listing
    }
  }
}";

const LISTING: &str = r"
fragment Listing on MergeRequestConnection {
  nodes {
    iid title draft
    author { username name }
    reviewers(first: 1) { nodes { username } }
    diffStatsSummary { additions deletions }
    targetProject { fullPath }
  }
  pageInfo { hasNextPage endCursor }
}";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Page {
    nodes: Vec<MergeRequest>,
    page_info: PageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergeRequest {
    iid: String,
    title: String,
    draft: bool,
    author: Option<WireUser>,
    reviewers: Reviewers,
    diff_stats_summary: Option<DiffStats>,
    target_project: Project,
}

#[derive(Deserialize)]
struct Reviewers {
    nodes: Vec<WireUser>,
}

#[derive(Deserialize)]
struct DiffStats {
    additions: u32,
    deletions: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    full_path: String,
}

impl MergeRequest {
    fn into_item(self, repo: Arc<Repo>) -> Result<PullRequestListItem> {
        let review_status = if self.draft {
            ReviewStatus::Draft
        } else if self.reviewers.nodes.is_empty() {
            ReviewStatus::NoDecision
        } else {
            ReviewStatus::ReviewRequired
        };

        Ok(PullRequestListItem {
            target: PullRequestTarget {
                repo,
                number: self
                    .iid
                    .parse()
                    .context("invalid merge request IID")?,
            },
            title: self.title,
            author: self
                .author
                .as_ref()
                .map(WireUser::display)
                .unwrap_or_default(),
            review_status,
            changes: self.diff_stats_summary.map(|stats| Changes {
                additions: stats.additions,
                deletions: stats.deletions,
            }),
        })
    }
}

pub async fn repository_pull_requests(repo: Repo) -> Result<PullRequestList> {
    let variables =
        json!({ "path": format!("{}/{}", repo.namespace, repo.name) });
    let requests = fetch(host_of(&repo), REPOSITORY_QUERY, variables).await?;
    let repo = Arc::new(repo);
    let items = requests
        .into_iter()
        .map(|mr| mr.into_item(Arc::clone(&repo)))
        .collect::<Result<_>>()?;

    Ok(PullRequestList {
        scope: PullRequestListScope::Repository,
        items,
    })
}

pub async fn user_pull_requests() -> Result<PullRequestList> {
    let host = std::env::var("GITLAB_HOST")
        .ok()
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_owned());
    let requests = fetch(&host, USER_QUERY, json!({})).await?;
    let items = requests
        .into_iter()
        .map(|mr| {
            let (namespace, name) = mr
                .target_project
                .full_path
                .rsplit_once('/')
                .filter(|(namespace, name)| {
                    !namespace.is_empty() && !name.is_empty()
                })
                .context("merge request has an invalid target project path")?;
            let repo = Repo {
                host: Some(host.clone()),
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            };

            mr.into_item(Arc::new(repo))
        })
        .collect::<Result<_>>()?;

    Ok(PullRequestList {
        scope: PullRequestListScope::User,
        items,
    })
}

async fn fetch(
    host: &str,
    query: &'static str,
    variables: Value,
) -> Result<Vec<MergeRequest>> {
    let token = transport::token(host).await;
    let url = format!("https://{host}/api/graphql");

    tokio::task::spawn_blocking(move || {
        read_all(&url, token.as_deref(), query, variables)
    })
    .await
    .context("request task failed")?
}

fn read_all(
    url: &str,
    token: Option<&str>,
    query: &str,
    mut variables: Value,
) -> Result<Vec<MergeRequest>> {
    let query = format!("{query}\n{LISTING}");
    variables["first"] = json!(PAGE_SIZE);
    variables["after"] = Value::Null;
    let mut items = Vec::new();
    let mut cursors = HashSet::new();

    loop {
        let mut response = transport::graphql(url, token, &query, &variables)?;
        let connection = response.pointer_mut("/data/scope/mergeRequests")
            .filter(|value| !value.is_null())
            .context("GitLab merge request listing is unavailable; check the project and authentication")?;
        let page: Page = serde_json::from_value(connection.take())
            .context("failed to parse GitLab merge request listing")?;
        items.extend(page.nodes);
        if !page.page_info.has_next_page {
            return Ok(items);
        }

        let cursor = page
            .page_info
            .end_cursor
            .filter(|cursor| !cursor.is_empty())
            .context("GitLab pagination is missing its next cursor")?;
        if !cursors.insert(cursor.clone()) {
            bail!("GitLab pagination did not advance");
        }
        variables["after"] = json!(cursor);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::time::Duration;

    fn merge_request() -> Value {
        json!({
            "iid": "42", "title": "Fix parser", "draft": false,
            "author": { "username": "alice", "name": "Alice" },
            "reviewers": { "nodes": [{ "username": "bob" }] },
            "diffStatsSummary": { "additions": 123, "deletions": 45 },
            "targetProject": { "fullPath": "group/sub/project" }
        })
    }

    fn page(
        nodes: &[Value],
        has_next_page: bool,
        end_cursor: Option<&str>,
    ) -> Value {
        json!({ "data": { "scope": { "mergeRequests": {
            "nodes": nodes,
            "pageInfo": { "hasNextPage": has_next_page, "endCursor": end_cursor }
        }}}})
    }

    fn server(
        responses: Vec<Value>,
    ) -> (String, std::thread::JoinHandle<Vec<Value>>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url =
            format!("http://{}/api/graphql", listener.local_addr().unwrap());
        let handle = std::thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(&stream);
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                assert!(line.starts_with("POST /api/graphql "));
                let mut length = 0;
                let mut has_auth = false;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let header = line.to_ascii_lowercase();
                    if let Some(value) = header.strip_prefix("content-length:")
                    {
                        length = value.trim().parse().unwrap();
                    }
                    has_auth |=
                        header == "authorization: bearer test-token\r\n";
                }
                assert!(has_auth);
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                requests.push(serde_json::from_slice(&body).unwrap());
                let body = response.to_string();
                write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            requests
        });
        (url, handle)
    }

    #[test]
    fn listings_preserve_counts_authors_and_review_status() {
        let repo = Arc::new(
            crate::parse_repo("gitlab.example.com/group/sub/project").unwrap(),
        );
        let parse = |value| {
            serde_json::from_value::<MergeRequest>(value)
                .unwrap()
                .into_item(Arc::clone(&repo))
                .unwrap()
        };
        let item = parse(merge_request());
        assert_eq!(item.target.number, 42);
        assert_eq!(
            item.target.repo.slug(),
            "gitlab.example.com/group/sub/project"
        );
        assert_eq!(item.author, "Alice");
        assert!(matches!(item.review_status, ReviewStatus::ReviewRequired));
        assert_eq!(
            item.changes,
            Some(Changes {
                additions: 123,
                deletions: 45
            })
        );

        let mut value = merge_request();
        value["draft"] = json!(true);
        value["author"]["name"] = Value::Null;
        value["diffStatsSummary"] = Value::Null;
        let item = parse(value);
        assert!(matches!(item.review_status, ReviewStatus::Draft));
        assert_eq!(item.author, "alice");
        assert_eq!(item.changes, None);

        let mut value = merge_request();
        value["author"] = Value::Null;
        value["reviewers"]["nodes"] = json!([]);
        value["diffStatsSummary"] = json!({ "additions": 0, "deletions": 0 });
        let item = parse(value);
        assert!(matches!(item.review_status, ReviewStatus::NoDecision));
        assert_eq!(item.author, "");
        assert_eq!(
            item.changes,
            Some(Changes {
                additions: 0,
                deletions: 0
            })
        );
    }

    #[test]
    fn both_listings_follow_cursors_beyond_one_hundred_results() {
        for query in [REPOSITORY_QUERY, USER_QUERY] {
            let (url, server) = server(vec![
                page(&vec![merge_request(); 100], true, Some("next")),
                page(&[merge_request()], false, None),
            ]);
            let items = read_all(
                &url,
                Some("test-token"),
                query,
                json!({ "path": "group/sub/project" }),
            )
            .unwrap();
            assert_eq!(items.len(), 101);
            assert_eq!(
                items[100].target_project.full_path,
                "group/sub/project"
            );
            let requests = server.join().unwrap();
            assert_eq!(requests[0]["variables"]["first"], 100);
            assert!(requests[0]["variables"]["after"].is_null());
            assert_eq!(requests[1]["variables"]["after"], "next");
            assert_eq!(requests[1]["variables"]["path"], "group/sub/project");
            assert!(
                requests[0]["query"]
                    .as_str()
                    .unwrap()
                    .contains("diffStatsSummary")
            );
        }
    }

    #[test]
    fn invalid_pagination_and_partial_responses_fail() {
        let mut partial = page(&[merge_request()], false, None);
        partial["errors"] = json!([{ "message": "Diff stats unavailable" }]);
        for (responses, expected) in [
            (vec![partial], "Diff stats unavailable"),
            (
                vec![json!({ "data": { "scope": null } })],
                "listing is unavailable",
            ),
            (vec![page(&[], true, None)], "missing its next cursor"),
            (vec![page(&[], true, Some(""))], "missing its next cursor"),
            (
                vec![
                    page(&[], true, Some("a")),
                    page(&[], true, Some("b")),
                    page(&[], true, Some("a")),
                ],
                "did not advance",
            ),
        ] {
            let (url, server) = server(responses);
            let error =
                read_all(&url, Some("test-token"), REPOSITORY_QUERY, json!({}))
                    .err()
                    .unwrap();
            assert!(error.to_string().contains(expected), "{error:#}");
            server.join().unwrap();
        }
    }
}
