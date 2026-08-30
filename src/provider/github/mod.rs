use crate::model::{
    AddedThread, ChangedFile, Meta, NewThread, Parent, ReviewEvent,
};
use crate::provider::{
    Check, CheckState, LocatedPullRequest, Provider, PullRequest,
    PullRequestList, Repo, ReviewStatus, Reviewer, Summary, Threads, Verdict,
};
use crate::text::url::escape_path;
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::sync::Arc;
use tokio::process::Command;
use ureq::http::Uri;

mod transport;
mod wire;

use transport::{
    Retry, check, get, graphql, next_page, post, token, write_token,
};
/// The first page of every connection the review needs.
///
/// A connection GitHub caps at 100 arrives short of the whole review, so each
/// one carries the cursor that reads the rest. Most reviews fit in one page and
/// pay for exactly this one round trip; see [`complete_meta`] for the overflow.
const THREADS_QUERY: &str = r"
query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      id number title state isDraft additions deletions changedFiles
      author { login }
      baseRefName headRefName headRefOid body
      files(first:100) {
        pageInfo { hasNextPage endCursor }
        nodes { path viewerViewedState }
      }
      pendingReview: reviews(first:1, states:[PENDING]) { nodes { id } }
      discussion: comments(first:100) {
        pageInfo { hasNextPage endCursor }
        nodes { id fullDatabaseId author { login } body createdAt }
      }
      reviewThreads(first:100) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated viewerCanResolve path subjectType
          line originalLine diffSide startLine startDiffSide
          comments(first:100) {
            pageInfo { hasNextPage endCursor }
            nodes { id fullDatabaseId state author { login } body createdAt }
          }
        }
      }
    }
  }
}
";

/// The later pages, one query per connection.
///
/// Each answers with the same field under the same path as [`THREADS_QUERY`],
/// so a page merges onto the first one without knowing which query fetched it.
const MORE_FILES_QUERY: &str = r"
query($owner:String!, $repo:String!, $number:Int!, $after:String!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      files(first:100, after:$after) {
        pageInfo { hasNextPage endCursor }
        nodes { path viewerViewedState }
      }
    }
  }
}
";

const MORE_DISCUSSION_QUERY: &str = r"
query($owner:String!, $repo:String!, $number:Int!, $after:String!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      discussion: comments(first:100, after:$after) {
        pageInfo { hasNextPage endCursor }
        nodes { id fullDatabaseId author { login } body createdAt }
      }
    }
  }
}
";

/// Later threads arrive with only their first page of comments, the same way
/// the first page of threads does, so both are drained by the same pass.
const MORE_THREADS_QUERY: &str = r"
query($owner:String!, $repo:String!, $number:Int!, $after:String!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      reviewThreads(first:100, after:$after) {
        pageInfo { hasNextPage endCursor }
        nodes {
          id isResolved isOutdated viewerCanResolve path subjectType
          line originalLine diffSide startLine startDiffSide
          comments(first:100) {
            pageInfo { hasNextPage endCursor }
            nodes { id fullDatabaseId state author { login } body createdAt }
          }
        }
      }
    }
  }
}
";

/// A thread's comments are addressed by the thread rather than by the pull
/// request, since that is the only way to name one connection out of a hundred.
const MORE_THREAD_COMMENTS_QUERY: &str = r"
query($id:ID!, $after:String!) {
  node(id:$id) {
    ... on PullRequestReviewThread {
      comments(first:100, after:$after) {
        pageInfo { hasNextPage endCursor }
        nodes { id fullDatabaseId state author { login } body createdAt }
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

/// Everything the selector's summary panel reads, in one round trip.
///
/// Connections are capped at their first page: the panel counts rather than
/// lists, and a review with more than a hundred threads says `100+` instead of
/// paying for pages nobody reads.
const SUMMARY_QUERY: &str = r"
query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      additions deletions changedFiles updatedAt
      author { login }
      baseRefName headRefName
      comments { totalCount }
      reviewRequests(first:100) {
        nodes {
          requestedReviewer {
            __typename
            ... on User { login }
            ... on Bot { login }
            ... on Mannequin { login }
            ... on Team { combinedSlug }
          }
        }
      }
      latestReviews(first:100) { nodes { state author { login } } }
      reviewThreads(first:100) {
        totalCount
        nodes { isResolved }
      }
      commits(last:1) {
        nodes {
          commit {
            statusCheckRollup {
              contexts(first:100) {
                nodes {
                  __typename
                  ... on CheckRun { name status conclusion }
                  ... on StatusContext { context state }
                }
              }
            }
          }
        }
      }
    }
  }
}
";

const USER_PULL_REQUESTS_QUERY: &str = r"
query($endCursor:String) {
  viewer {
    pullRequests(
      first:100
      after:$endCursor
      states:OPEN
      orderBy:{field:UPDATED_AT,direction:DESC}
    ) {
      nodes {
        number title isDraft reviewDecision
        repository { nameWithOwner }
      }
      pageInfo { hasNextPage endCursor }
    }
  }
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

const JSON_ACCEPT: &str = "application/vnd.github+json";
const API_LIMIT: u64 = 64 * 1024 * 1024;

/// Serves a blob as itself rather than as base64 inside a JSON envelope.
const RAW_ACCEPT: &str = "application/vnd.github.raw";

/// A file big enough to exceed this is not one anybody expands into a terminal
/// pane, and holding it would cost more than the diff it decorates.
const BLOB_LIMIT: u64 = 8 * 1024 * 1024;

const STATUS_URL: &str = "https://www.githubstatus.com/api/v2/components.json";

/// The two components a review rides on. A degraded Actions or Pages says
/// nothing about why a diff would not load, and naming it would only mislead.
const STATUS_COMPONENTS: [&str; 2] = ["API Requests", "Pull Requests"];

/// GitHub's implementation of the code-review provider boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitHub;

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
            namespace: owner.to_string(),
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
    fn web_url(&self) -> String {
        let host = self.host.as_deref().unwrap_or("github.com");

        format!("https://{host}/{}/{}", self.namespace, self.name)
    }

    fn graphql_url(&self) -> String {
        match self.enterprise_host() {
            Some(host) => format!("https://{host}/api/graphql"),
            None => "https://api.github.com/graphql".to_string(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ReviewDecision {
    Approved,
    ChangesRequested,
    ReviewRequired,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRepositoryPullRequest {
    number: u32,
    title: String,
    is_draft: bool,
    #[serde(deserialize_with = "deserialize_cli_review_decision")]
    review_decision: Option<ReviewDecision>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireUserPullRequest {
    number: u32,
    title: String,
    is_draft: bool,
    review_decision: Option<ReviewDecision>,
    repository: WireRepository,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRepository {
    name_with_owner: String,
}

#[derive(Deserialize)]
struct WireUserPullRequestPage {
    data: WireUserPullRequestData,
}

#[derive(Deserialize)]
struct WireUserPullRequestData {
    viewer: WireUserPullRequests,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireUserPullRequests {
    pull_requests: WireUserPullRequestConnection,
}

#[derive(Deserialize)]
struct WireUserPullRequestConnection {
    nodes: Vec<WireUserPullRequest>,
}

/// The summary response, deserialized rather than hand-walked so a field the
/// API stops sending fails the parse instead of counting as zero.
#[derive(Deserialize)]
struct WireSummaryResponse {
    data: WireSummaryData,
}

#[derive(Deserialize)]
struct WireSummaryData {
    repository: Option<WireSummaryRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSummaryRepository {
    pull_request: Option<WireSummary>,
}

#[derive(Deserialize)]
struct WireNodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireTotal {
    total_count: u32,
}

#[derive(Deserialize)]
struct WireLogin {
    login: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSummary {
    additions: u32,
    deletions: u32,
    changed_files: u32,
    updated_at: String,
    author: Option<WireLogin>,
    base_ref_name: String,
    head_ref_name: String,
    comments: WireTotal,
    review_requests: WireNodes<WireReviewRequest>,
    latest_reviews: Option<WireNodes<WireLatestReview>>,
    review_threads: WireSummaryThreads,
    commits: WireNodes<WireSummaryCommit>,
}

#[derive(Deserialize)]
struct WireLatestReview {
    state: String,
    author: Option<WireLogin>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireReviewRequest {
    requested_reviewer: Option<WireRequestedReviewer>,
}

/// A review can be asked of a person, of a bot, or of a whole team, and the
/// three name themselves differently.
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum WireRequestedReviewer {
    User {
        login: String,
    },
    Bot {
        login: String,
    },
    Mannequin {
        login: String,
    },
    Team {
        #[serde(rename = "combinedSlug")]
        combined_slug: String,
    },
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSummaryThreads {
    total_count: u32,
    nodes: Vec<WireSummaryThread>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireSummaryThread {
    is_resolved: bool,
}

#[derive(Deserialize)]
struct WireSummaryCommit {
    commit: WireRollupHolder,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRollupHolder {
    status_check_rollup: Option<WireRollup>,
}

#[derive(Deserialize)]
struct WireRollup {
    contexts: WireNodes<WireContext>,
}

/// A rollup mixes the checks an app reports with the statuses a commit carries,
/// and the two spell both their name and their verdict differently.
#[derive(Deserialize)]
#[serde(tag = "__typename")]
enum WireContext {
    CheckRun {
        name: String,
        status: String,
        conclusion: Option<String>,
    },
    StatusContext {
        context: String,
        state: String,
    },
    #[serde(other)]
    Other,
}

/// Failing checks first, then whatever is still running: a reader deciding
/// whether to review reads the top of the list and stops.
fn read_checks(contexts: Vec<WireContext>) -> Vec<Check> {
    let mut checks: Vec<Check> = contexts
        .into_iter()
        .filter_map(|context| match context {
            WireContext::CheckRun {
                name,
                status,
                conclusion,
            } => {
                let state = if status == "COMPLETED" {
                    match conclusion.as_deref() {
                        Some("SUCCESS") => CheckState::Passed,
                        Some("SKIPPED" | "NEUTRAL") => CheckState::Skipped,
                        _ => CheckState::Failed,
                    }
                } else {
                    CheckState::Running
                };

                Some(Check { name, state })
            }
            WireContext::StatusContext { context, state } => {
                let state = match state.as_str() {
                    "SUCCESS" => CheckState::Passed,
                    "PENDING" | "EXPECTED" => CheckState::Running,
                    _ => CheckState::Failed,
                };

                Some(Check {
                    name: context,
                    state,
                })
            }
            WireContext::Other => None,
        })
        .collect();

    checks.sort_by(|left, right| {
        left.state
            .cmp(&right.state)
            .then(left.name.cmp(&right.name))
    });

    checks
}

/// Everyone who has answered, then everyone still being waited on. A team is
/// listed as the team: nobody on it has been picked yet.
fn read_reviewers(
    reviews: Vec<WireLatestReview>,
    requests: Vec<WireReviewRequest>,
) -> Vec<Reviewer> {
    let answered = reviews.into_iter().filter_map(|review| {
        let verdict = match review.state.as_str() {
            "APPROVED" => Verdict::Approved,
            "CHANGES_REQUESTED" => Verdict::ChangesRequested,
            "COMMENTED" => Verdict::Commented,
            _ => return None,
        };

        Some(Reviewer {
            name: review.author?.login,
            is_team: false,
            verdict,
        })
    });

    let waiting = requests.into_iter().filter_map(|request| {
        let (name, is_team) = match request.requested_reviewer? {
            WireRequestedReviewer::User { login }
            | WireRequestedReviewer::Bot { login }
            | WireRequestedReviewer::Mannequin { login } => (login, false),
            WireRequestedReviewer::Team { combined_slug } => {
                (combined_slug, true)
            }
            WireRequestedReviewer::Other => return None,
        };

        Some(Reviewer {
            name,
            is_team,
            verdict: Verdict::Waiting,
        })
    });

    let mut reviewers: Vec<Reviewer> = answered.chain(waiting).collect();
    reviewers.sort_by(|left, right| {
        left.verdict
            .cmp(&right.verdict)
            .then(left.name.cmp(&right.name))
    });

    reviewers
}

fn parse_summary(val: &serde_json::Value) -> Result<Summary> {
    let response = WireSummaryResponse::deserialize(val)
        .context("unexpected pull request summary response")?;
    let pr = response
        .data
        .repository
        .and_then(|repository| repository.pull_request)
        .context("PR not found in graphql response")?;

    let threads = Threads {
        unresolved: pr
            .review_threads
            .nodes
            .iter()
            .filter(|thread| !thread.is_resolved)
            .count()
            .try_into()
            .unwrap_or(u32::MAX),
        total: pr.review_threads.total_count,
        is_truncated: pr.review_threads.total_count as usize
            > pr.review_threads.nodes.len(),
    };
    let checks = pr
        .commits
        .nodes
        .into_iter()
        .next()
        .and_then(|node| node.commit.status_check_rollup)
        .map(|rollup| read_checks(rollup.contexts.nodes))
        .unwrap_or_default();

    Ok(Summary {
        author: pr.author.map(|author| author.login).unwrap_or_default(),
        base_ref: pr.base_ref_name,
        head_ref: pr.head_ref_name,
        additions: pr.additions,
        deletions: pr.deletions,
        changed_files: pr.changed_files,
        updated_on: pr.updated_at.get(..10).unwrap_or_default().to_owned(),
        comments: pr.comments.total_count,
        checks,
        reviewers: read_reviewers(
            pr.latest_reviews
                .map(|reviews| reviews.nodes)
                .unwrap_or_default(),
            pr.review_requests.nodes,
        ),
        threads,
    })
}

fn deserialize_cli_review_decision<'de, D>(
    deserializer: D,
) -> std::result::Result<Option<ReviewDecision>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let decision = Option::<String>::deserialize(deserializer)?;

    match decision.as_deref() {
        None | Some("") => Ok(None),
        Some("APPROVED") => Ok(Some(ReviewDecision::Approved)),
        Some("CHANGES_REQUESTED") => Ok(Some(ReviewDecision::ChangesRequested)),
        Some("REVIEW_REQUIRED") => Ok(Some(ReviewDecision::ReviewRequired)),
        Some(other) => Err(serde::de::Error::unknown_variant(
            other,
            &["APPROVED", "CHANGES_REQUESTED", "REVIEW_REQUIRED"],
        )),
    }
}

const fn review_status(
    is_draft: bool,
    decision: Option<&ReviewDecision>,
) -> ReviewStatus {
    if is_draft {
        return ReviewStatus::Draft;
    }

    match decision {
        Some(ReviewDecision::ChangesRequested) => {
            ReviewStatus::ChangesRequested
        }
        Some(ReviewDecision::ReviewRequired) => ReviewStatus::ReviewRequired,
        Some(ReviewDecision::Approved) => ReviewStatus::Approved,
        None => ReviewStatus::NoDecision,
    }
}

const fn pull_request(
    number: u32,
    title: String,
    is_draft: bool,
    decision: Option<&ReviewDecision>,
) -> PullRequest {
    PullRequest {
        number,
        title,
        review_status: review_status(is_draft, decision),
    }
}

fn parse_repository_pull_requests(
    repo: Repo,
    bytes: &[u8],
) -> Result<PullRequestList> {
    let pulls: Vec<WireRepositoryPullRequest> =
        serde_json::from_slice(bytes)
            .context("failed to parse gh pr list output")?;

    Ok(PullRequestList::Repository {
        repo,
        pulls: pulls
            .into_iter()
            .map(|pull| {
                pull_request(
                    pull.number,
                    pull.title,
                    pull.is_draft,
                    pull.review_decision.as_ref(),
                )
            })
            .collect(),
    })
}

fn parse_user_pull_requests(bytes: &[u8]) -> Result<PullRequestList> {
    let pages: Vec<WireUserPullRequestPage> = serde_json::from_slice(bytes)
        .context("failed to parse user pull requests response")?;
    let pulls = pages
        .into_iter()
        .flat_map(|page| page.data.viewer.pull_requests.nodes)
        .map(|pull| {
            Ok(LocatedPullRequest {
                repo: Repo::parse(&pull.repository.name_with_owner)?,
                pull: pull_request(
                    pull.number,
                    pull.title,
                    pull.is_draft,
                    pull.review_decision.as_ref(),
                ),
            })
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(PullRequestList::User { pulls })
}

/// Decodes pages in the same shape as `gh api --paginate --slurp`.
///
/// The byte-oriented entry point keeps `serde_json::Value` inside the GitHub
/// boundary while allowing captured API responses to exercise the real wire
/// decoder.
pub fn parse_files(bytes: &[u8]) -> Result<Vec<ChangedFile>> {
    wire::files(bytes)
}

/// Decodes a captured metadata response through the production wire schema.
pub fn parse_meta(bytes: &[u8]) -> Result<Meta> {
    wire::meta_bytes(bytes)
}

/// Changed files with their unified-diff patches. Measured faster than the
/// `Accept: v3.diff` endpoint, and arrives pre-split per file.
async fn fetch_files(repo: &Repo, number: u32) -> Result<Vec<ChangedFile>> {
    let token = token(repo.host.as_deref()).await;
    let first = repo.rest_url(&format!(
        "/repos/{}/{}/pulls/{number}/files?per_page=100",
        repo.namespace, repo.name
    ));
    let origin: Uri = first.parse().context("invalid GitHub API URL")?;

    tokio::task::spawn_blocking(move || {
        let mut files = Vec::with_capacity(100);
        let mut next = Some(first);

        while let Some(url) = next {
            let mut response = get(&url, JSON_ACCEPT, token.as_deref())?;
            check(&mut response, "fetching changed files")?;
            next = next_page(response.headers(), &origin)?;

            let bytes = response
                .body_mut()
                .with_config()
                .limit(API_LIMIT)
                .read_to_vec()
                .context("failed to read /files response")?;

            files.extend(wire::file_page(&bytes)?);
        }

        Ok(files)
    })
    .await
    .context("changed-file fetch panicked")?
}

/// Where a connection continues, or `None` when the page in hand is the last.
fn next_cursor(connection: &serde_json::Value) -> Option<String> {
    let info = connection.get("pageInfo")?;

    if !info.get("hasNextPage")?.as_bool()? {
        return None;
    }

    info.get("endCursor")?.as_str().map(str::to_owned)
}

/// Reads a connection to its end, appending each later page onto the first.
///
/// `fetch` is handed a cursor and answers with the same connection one page
/// further on. The page's own `pageInfo` replaces the one it continues, which
/// is what ends the walk.
fn drain(
    connection: &mut serde_json::Value,
    mut fetch: impl FnMut(&str) -> Result<serde_json::Value>,
) -> Result<()> {
    while let Some(cursor) = next_cursor(connection) {
        let mut page = fetch(&cursor)?;

        let serde_json::Value::Array(nodes) = page["nodes"].take() else {
            bail!("a later page arrived with no nodes");
        };

        let Some(held) = connection["nodes"].as_array_mut() else {
            bail!("a connection arrived with no nodes");
        };

        held.extend(nodes);
        connection["pageInfo"] = page["pageInfo"].take();
    }

    Ok(())
}

/// Fills in every connection [`THREADS_QUERY`] cut short.
///
/// GitHub caps a connection at 100 nodes, so a review with more files,
/// conversations or comments than that arrives truncated and silently
/// disagrees with what the browser shows. Only the overflow costs a round
/// trip: a review that fits in one page leaves here untouched.
fn complete_meta(
    url: &str,
    token: Option<&str>,
    variables: &serde_json::Value,
    value: &mut serde_json::Value,
) -> Result<()> {
    let pr = &mut value["data"]["repository"]["pullRequest"];
    if pr.is_null() {
        return Ok(());
    }

    let page = |query: &'static str, what: &'static str, after: &str| {
        let mut variables = variables.clone();
        variables["after"] = after.into();

        graphql(url, token, query, &variables, what, Retry::Transient)
    };

    for (field, query, what) in [
        ("files", MORE_FILES_QUERY, "fetching the changed file list"),
        (
            "discussion",
            MORE_DISCUSSION_QUERY,
            "fetching pull request comments",
        ),
        (
            "reviewThreads",
            MORE_THREADS_QUERY,
            "fetching review threads",
        ),
    ] {
        drain(&mut pr[field], |after| {
            let mut answer = page(query, what, after)?;

            Ok(answer["data"]["repository"]["pullRequest"][field].take())
        })?;
    }

    let Some(threads) = pr["reviewThreads"]["nodes"].as_array_mut() else {
        return Ok(());
    };

    for thread in threads {
        let Some(id) = thread["id"].as_str().map(str::to_owned) else {
            continue;
        };

        drain(&mut thread["comments"], |after| {
            let variables = serde_json::json!({ "id": &id, "after": after });
            let mut answer = graphql(
                url,
                token,
                MORE_THREAD_COMMENTS_QUERY,
                &variables,
                "fetching a conversation",
                Retry::Transient,
            )?;

            Ok(answer["data"]["node"]["comments"].take())
        })?;
    }

    Ok(())
}

/// PR metadata plus review threads, in one round trip plus whatever the caps
/// on its connections left behind.
async fn fetch_meta(repo: &Repo, number: u32) -> Result<Meta> {
    let token = token(repo.host.as_deref()).await;
    let url = repo.graphql_url();
    let variables = serde_json::json!({
        "owner": repo.namespace,
        "repo": repo.name,
        "number": number,
    });

    tokio::task::spawn_blocking(move || {
        let mut value = graphql(
            &url,
            token.as_deref(),
            THREADS_QUERY,
            &variables,
            "fetching pull request metadata",
            Retry::Transient,
        )?;

        complete_meta(&url, token.as_deref(), &variables, &mut value)?;

        wire::meta(value)
    })
    .await
    .context("metadata fetch panicked")?
}

/// One file at one commit, which is what fills a gap the patch left out.
///
/// The contents endpoint serves the blob directly under the raw media type, so
/// nothing has to be pulled back out of a JSON envelope on the way in.
async fn fetch_blob(repo: &Repo, path: &str, commit: &str) -> Result<String> {
    let token = token(repo.host.as_deref()).await;
    let url = repo.rest_url(&format!(
        "/repos/{}/{}/contents/{}?ref={commit}",
        repo.namespace,
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
        graphql(
            &url,
            Some(token.as_ref()),
            query,
            &variables,
            what,
            Retry::Refusals,
        )
    })
    .await
    .context("mutation panicked")?
}

/// Files one draft comment against the pending review, opening that review when
/// its typed target is the pull request. The wire response is decoded before
/// it crosses back into the runtime.
async fn add_thread(repo: &Repo, thread: NewThread) -> Result<AddedThread> {
    let value = mutate(
        repo,
        ADD_THREAD_MUTATION,
        wire::thread_variables(thread),
        "saving draft",
    )
    .await?;

    wire::added_thread(value)
}

async fn update_comment(
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

async fn delete_comment(repo: &Repo, comment: Arc<str>) -> Result<()> {
    mutate(
        repo,
        DELETE_COMMENT_MUTATION,
        serde_json::json!({ "id": &*comment }),
        "discarding draft",
    )
    .await
    .map(|_| ())
}

/// Publishes an existing pending review or creates a verdict-only review when
/// no draft opened one. The target determines the mutation and wire field.
async fn submit_review(
    repo: &Repo,
    parent: Parent,
    event: ReviewEvent,
    body: String,
) -> Result<()> {
    let query = match parent {
        Parent::Review(_) => SUBMIT_REVIEW_MUTATION,
        Parent::PullRequest(_) => CREATE_REVIEW_MUTATION,
    };
    let variables = wire::review_variables(parent, event, body);

    mutate(repo, query, variables, "submitting review")
        .await
        .map(|_| ())
}

/// A reply is a standalone comment addressed to the thread's first comment; it
/// posts immediately rather than waiting for a review to be submitted.
async fn reply(
    repo: &Repo,
    number: u32,
    in_reply_to: u64,
    body: String,
) -> Result<()> {
    let token = write_token(repo).await?;
    let url = repo.rest_url(&format!(
        "/repos/{}/{}/pulls/{number}/comments",
        repo.namespace, repo.name
    ));
    let payload =
        serde_json::json!({ "body": body, "in_reply_to": in_reply_to });

    tokio::task::spawn_blocking(move || {
        let mut response =
            post(&url, Some(token.as_ref()), &payload, Retry::Refusals)?;
        check(&mut response, "posting reply")
    })
    .await
    .context("reply panicked")?
}

async fn set_resolved(
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
async fn set_viewed(
    repo: &Repo,
    pr: Arc<str>,
    path: &str,
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
        serde_json::json!({ "id": &*pr, "path": path }),
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
async fn fetch_outage(repo: &Repo) -> Option<String> {
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

async fn gh_output(args: &[&str], failure: &str) -> Result<Vec<u8>> {
    let output = Command::new("gh")
        .args(args)
        .output()
        .await
        .context("failed to spawn gh; is it installed and on PATH?")?;

    if !output.status.success() {
        let error = String::from_utf8_lossy(&output.stderr);
        bail!("{failure}: {}", error.trim());
    }

    Ok(output.stdout)
}

async fn repository_pull_requests(repo: Repo) -> Result<PullRequestList> {
    let slug = repo.slug();
    let output = gh_output(
        &[
            "pr",
            "list",
            "--repo",
            &slug,
            "--state",
            "open",
            "--limit",
            "100",
            "--json",
            "number,title,isDraft,reviewDecision",
        ],
        "gh pr list failed",
    )
    .await?;

    parse_repository_pull_requests(repo, &output)
}

/// The summary panel's one round trip, asked for only when the panel is
/// opened on a pull request it has not read yet.
async fn fetch_summary(repo: &Repo, number: u32) -> Result<Summary> {
    let token = token(repo.host.as_deref()).await;
    let url = repo.graphql_url();
    let variables = serde_json::json!({
        "owner": repo.namespace,
        "repo": repo.name,
        "number": number,
    });

    tokio::task::spawn_blocking(move || {
        let val = graphql(
            &url,
            token.as_deref(),
            SUMMARY_QUERY,
            &variables,
            "fetching the pull request summary",
            Retry::Transient,
        )?;

        parse_summary(&val)
    })
    .await
    .context("summary fetch panicked")?
}

async fn user_pull_requests() -> Result<PullRequestList> {
    let query = format!("query={USER_PULL_REQUESTS_QUERY}");
    let output = gh_output(
        &[
            "api",
            "graphql",
            "--paginate",
            "--slurp",
            "--raw-field",
            &query,
        ],
        "gh api graphql failed",
    )
    .await?;

    parse_user_pull_requests(&output)
}

/// Returns the current GitHub repository when the process is inside a Git
/// worktree.
async fn current_repo_if_present() -> Result<Option<Repo>> {
    let git = Command::new("git")
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .await
        .context("failed to spawn git; is it installed and on PATH?")?;

    if !git.status.success()
        || String::from_utf8_lossy(&git.stdout).trim() != "true"
    {
        return Ok(None);
    }

    current_repo().await.map(Some)
}

/// Resolved from the local git remotes, which is the CLI's job rather than an
/// API call.
async fn current_repo() -> Result<Repo> {
    // The web URL rather than `nameWithOwner`, which names the repository but
    // not the host it lives on. Dropping the host sent an enterprise checkout
    // to github.com.
    let output = gh_output(
        &["repo", "view", "--json", "url", "--jq", ".url"],
        "gh repo view failed",
    )
    .await?;

    Repo::from_url(String::from_utf8_lossy(&output).trim())
}

impl Provider for GitHub {
    fn parse_repo(self, slug: &str) -> Result<Repo> {
        Repo::parse(slug)
    }

    fn repo_url(self, repo: &Repo) -> String {
        repo.web_url()
    }

    async fn current_repo_if_present(self) -> Result<Option<Repo>> {
        current_repo_if_present().await
    }

    async fn repository_pull_requests(
        self,
        repo: Repo,
    ) -> Result<PullRequestList> {
        repository_pull_requests(repo).await
    }

    async fn user_pull_requests(self) -> Result<PullRequestList> {
        user_pull_requests().await
    }

    async fn fetch_summary(self, repo: &Repo, number: u32) -> Result<Summary> {
        fetch_summary(repo, number).await
    }

    async fn fetch_files(
        self,
        repo: &Repo,
        number: u32,
    ) -> Result<Vec<ChangedFile>> {
        fetch_files(repo, number).await
    }

    async fn fetch_meta(self, repo: &Repo, number: u32) -> Result<Meta> {
        fetch_meta(repo, number).await
    }

    async fn fetch_blob(
        self,
        repo: &Repo,
        path: &str,
        commit: &str,
    ) -> Result<String> {
        fetch_blob(repo, path, commit).await
    }

    async fn add_thread(
        self,
        repo: &Repo,
        thread: NewThread,
    ) -> Result<AddedThread> {
        add_thread(repo, thread).await
    }

    async fn update_comment(
        self,
        repo: &Repo,
        comment: Arc<str>,
        body: String,
    ) -> Result<()> {
        update_comment(repo, comment, body).await
    }

    async fn delete_comment(
        self,
        repo: &Repo,
        comment: Arc<str>,
    ) -> Result<()> {
        delete_comment(repo, comment).await
    }

    async fn submit_review(
        self,
        repo: &Repo,
        parent: Parent,
        event: ReviewEvent,
        body: String,
    ) -> Result<()> {
        submit_review(repo, parent, event, body).await
    }

    async fn reply(
        self,
        repo: &Repo,
        number: u32,
        in_reply_to: u64,
        body: String,
    ) -> Result<()> {
        reply(repo, number, in_reply_to, body).await
    }

    async fn set_resolved(
        self,
        repo: &Repo,
        thread_id: Arc<str>,
        is_resolved: bool,
    ) -> Result<()> {
        set_resolved(repo, thread_id, is_resolved).await
    }

    async fn set_viewed(
        self,
        repo: &Repo,
        pr: Arc<str>,
        path: &str,
        is_viewed: bool,
    ) -> Result<()> {
        set_viewed(repo, pr, path, is_viewed).await
    }

    async fn fetch_outage(self, repo: &Repo) -> Option<String> {
        fetch_outage(repo).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_is_usable_through_the_provider_boundary() {
        let provider = GitHub;
        let repo = provider.parse_repo("owner/repo").unwrap();

        assert_eq!(repo.slug(), "owner/repo");
        assert_eq!(provider.repo_url(&repo), "https://github.com/owner/repo");
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
        assert_eq!(public.namespace, "cli");
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

    /// A connection as GitHub sends it: the nodes in hand, and whether more
    /// of them are waiting behind a cursor.
    fn connection(nodes: &[u32], after: Option<&str>) -> serde_json::Value {
        serde_json::json!({
            "pageInfo": {
                "hasNextPage": after.is_some(),
                "endCursor": after,
            },
            "nodes": nodes,
        })
    }

    fn node_values(connection: &serde_json::Value) -> Vec<u32> {
        connection["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|node| node.as_u64().unwrap() as u32)
            .collect()
    }

    /// The common case, and the one that has to stay free: a review that fits
    /// inside one page asks GitHub for nothing more.
    #[test]
    fn a_connection_that_fits_in_one_page_costs_no_round_trip() {
        let mut held = connection(&[1, 2, 3], None);

        drain(&mut held, |_| panic!("fetched a page that was not needed"))
            .unwrap();

        assert_eq!(node_values(&held), [1, 2, 3]);
    }

    /// Every page after the first is appended in the order it arrives, and the
    /// cursor each one carries is what says whether to keep going.
    #[test]
    fn a_capped_connection_is_read_to_its_end() {
        let mut held = connection(&[1, 2], Some("one"));
        let mut asked = Vec::new();

        drain(&mut held, |cursor| {
            asked.push(cursor.to_owned());

            Ok(match cursor {
                "one" => connection(&[3, 4], Some("two")),
                _ => connection(&[5], None),
            })
        })
        .unwrap();

        assert_eq!(asked, ["one", "two"]);
        assert_eq!(node_values(&held), [1, 2, 3, 4, 5]);
        assert_eq!(next_cursor(&held), None);
    }

    /// A page that came back the wrong shape has to say so. Dropping it would
    /// silently truncate the review, which is the defect being fixed.
    #[test]
    fn a_page_that_arrives_malformed_fails_rather_than_truncating() {
        let mut held = connection(&[1], Some("one"));

        let error = drain(&mut held, |_| Ok(serde_json::json!({})))
            .unwrap_err()
            .to_string();

        assert!(error.contains("no nodes"), "{error}");
    }

    /// A connection GitHub answers without a cursor is the whole of it. The
    /// walk must end rather than read a missing field as another page.
    #[test]
    fn a_connection_with_no_page_info_is_taken_as_complete() {
        assert_eq!(next_cursor(&serde_json::json!({ "nodes": [] })), None);
        assert_eq!(next_cursor(&serde_json::json!(null)), None);
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

    #[test]
    fn parses_repository_pull_requests_and_moves_the_repository_once() {
        let local = parse_repository_pull_requests(
            Repo::parse("owner/repo").unwrap(),
            br#"[
                {"number":12,"title":"Draft","isDraft":true,"reviewDecision":null},
                {"number":13,"title":"Ready","isDraft":false,"reviewDecision":"APPROVED"},
                {"number":14,"title":"Changes","isDraft":false,"reviewDecision":"CHANGES_REQUESTED"},
                {"number":15,"title":"Review","isDraft":false,"reviewDecision":"REVIEW_REQUIRED"},
                {"number":16,"title":"Unreviewed","isDraft":false,"reviewDecision":""}
            ]"#,
        )
        .unwrap();
        assert!(!local.shows_repositories());
        assert_eq!(local.row(0).unwrap().number, 12);
        assert!(matches!(
            local.row(0).unwrap().review_status,
            ReviewStatus::Draft
        ));
        assert!(matches!(
            local.row(1).unwrap().review_status,
            ReviewStatus::Approved
        ));
        assert!(matches!(
            local.row(2).unwrap().review_status,
            ReviewStatus::ChangesRequested
        ));
        assert!(matches!(
            local.row(3).unwrap().review_status,
            ReviewStatus::ReviewRequired
        ));
        assert!(matches!(
            local.row(4).unwrap().review_status,
            ReviewStatus::NoDecision
        ));

        let target = local.select(1).unwrap();
        assert_eq!(target.repo.slug(), "owner/repo");
        assert_eq!(target.number, 13);
    }

    #[test]
    fn parses_user_pull_requests_and_moves_the_selected_repository() {
        let global = parse_user_pull_requests(
            br#"[{
                "data": {
                    "viewer": {
                        "pullRequests": {
                            "nodes": [{
                                "number": 34,
                                "title": "Global change",
                                "isDraft": false,
                                "reviewDecision": "CHANGES_REQUESTED",
                                "repository": {
                                    "nameWithOwner": "other/repo"
                                }
                            }],
                            "pageInfo": {
                                "hasNextPage": false,
                                "endCursor": null
                            }
                        }
                    }
                }
            }]"#,
        )
        .unwrap();
        assert!(global.shows_repositories());
        let row = global.row(0).unwrap();
        assert_eq!(row.repository.unwrap().slug(), "other/repo");
        assert_eq!(row.title, "Global change");
        assert!(matches!(row.review_status, ReviewStatus::ChangesRequested));

        let target = global.select(0).unwrap();
        assert_eq!(target.repo.slug(), "other/repo");
        assert_eq!(target.number, 34);
    }

    #[test]
    fn a_summary_counts_the_checks_the_reviewers_and_the_threads() {
        let val: serde_json::Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequest":{
                "additions":838,"deletions":55,"changedFiles":4,
                "updatedAt":"2026-08-28T20:04:55Z",
                "author":{"login":"tale"},
                "baseRefName":"main","headRefName":"rows",
                "comments":{"totalCount":2},
                "reviewRequests":{"nodes":[
                    {"requestedReviewer":{"__typename":"User",
                     "login":"dana"}},
                    {"requestedReviewer":{"__typename":"Team",
                     "combinedSlug":"owner/backend"}},
                    {"requestedReviewer":null}
                ]},
                "latestReviews":{"nodes":[
                    {"state":"APPROVED","author":{"login":"alice"}},
                    {"state":"APPROVED","author":{"login":"erin"}},
                    {"state":"CHANGES_REQUESTED","author":{"login":"bob"}},
                    {"state":"DISMISSED","author":{"login":"carol"}}
                ]},
                "reviewThreads":{"totalCount":3,"nodes":[
                    {"isResolved":false},
                    {"isResolved":true},
                    {"isResolved":false}
                ]},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":{
                    "contexts":{"nodes":[
                        {"__typename":"CheckRun","name":"build",
                         "status":"COMPLETED","conclusion":"SUCCESS"},
                        {"__typename":"CheckRun","name":"deploy",
                         "status":"IN_PROGRESS","conclusion":null},
                        {"__typename":"CheckRun","name":"clippy",
                         "status":"COMPLETED","conclusion":"FAILURE"},
                        {"__typename":"CheckRun","name":"docs",
                         "status":"COMPLETED","conclusion":"SKIPPED"},
                        {"__typename":"StatusContext","context":"vercel",
                         "state":"PENDING"}
                    ]}
                }}}]}
            }}}}"#,
        )
        .unwrap();

        let summary = parse_summary(&val).unwrap();

        assert_eq!(summary.author, "tale");
        assert_eq!(summary.updated_on, "2026-08-28");

        // Failing first, then running, then the rest by name.
        let checks: Vec<(&str, CheckState)> = summary
            .checks
            .iter()
            .map(|check| (check.name.as_str(), check.state))
            .collect();
        assert_eq!(
            checks,
            [
                ("clippy", CheckState::Failed),
                ("deploy", CheckState::Running),
                ("vercel", CheckState::Running),
                ("build", CheckState::Passed),
                ("docs", CheckState::Skipped),
            ]
        );

        // A dismissed review is nobody's verdict, and a team is named as the
        // team it was asked of.
        let reviewers: Vec<(&str, Verdict, bool)> = summary
            .reviewers
            .iter()
            .map(|reviewer| {
                (reviewer.name.as_str(), reviewer.verdict, reviewer.is_team)
            })
            .collect();
        assert_eq!(
            reviewers,
            [
                ("bob", Verdict::ChangesRequested, false),
                ("dana", Verdict::Waiting, false),
                ("owner/backend", Verdict::Waiting, true),
                ("alice", Verdict::Approved, false),
                ("erin", Verdict::Approved, false),
            ]
        );
        assert_eq!(summary.threads.unresolved, 2);
        assert_eq!(summary.threads.total, 3);
        assert!(!summary.threads.is_truncated);
    }

    /// A pull request nothing has run against still has to summarize.
    #[test]
    fn a_summary_survives_an_empty_rollup() {
        let val: serde_json::Value = serde_json::from_str(
            r#"{"data":{"repository":{"pullRequest":{
                "additions":1,"deletions":0,"changedFiles":1,
                "updatedAt":"2026-08-28T20:04:55Z",
                "author":null,
                "baseRefName":"main","headRefName":"rows",
                "comments":{"totalCount":0},
                "reviewRequests":{"nodes":[]},
                "latestReviews":{"nodes":[]},
                "reviewThreads":{"totalCount":0,"nodes":[]},
                "commits":{"nodes":[{"commit":{"statusCheckRollup":null}}]}
            }}}}"#,
        )
        .unwrap();

        let summary = parse_summary(&val).unwrap();

        assert!(summary.checks.is_empty());
        assert!(summary.reviewers.is_empty());
        assert_eq!(summary.author, "");
    }

    /// More threads than the one page counted makes the tally a floor, which
    /// the panel says out loud rather than reporting a wrong number.
    #[test]
    fn a_thread_page_short_of_the_total_is_marked_truncated() {
        let nodes = (0..100)
            .map(|_| r#"{"isResolved":false}"#)
            .collect::<Vec<_>>()
            .join(",");
        let val: serde_json::Value = serde_json::from_str(&format!(
            r#"{{"data":{{"repository":{{"pullRequest":{{
                "additions":1,"deletions":0,"changedFiles":1,
                "updatedAt":"2026-08-28T20:04:55Z",
                "author":{{"login":"tale"}},
                "baseRefName":"main","headRefName":"rows",
                "comments":{{"totalCount":0}},
                "reviewRequests":{{"nodes":[]}},
                "latestReviews":{{"nodes":[]}},
                "reviewThreads":{{"totalCount":140,"nodes":[{nodes}]}},
                "commits":{{"nodes":[]}}
            }}}}}}}}"#
        ))
        .unwrap();

        let summary = parse_summary(&val).unwrap();

        assert_eq!(summary.threads.unresolved, 100);
        assert_eq!(summary.threads.total, 140);
        assert!(summary.threads.is_truncated);
    }

    #[test]
    fn rejects_unknown_review_decisions() {
        let local = parse_repository_pull_requests(
            Repo::parse("owner/repo").unwrap(),
            br#"[{"number":1,"title":"Unknown","isDraft":false,
                "reviewDecision":"QUEUED"}]"#,
        );
        assert!(local.is_err());

        let global = parse_user_pull_requests(
            br#"[{"data":{"viewer":{"pullRequests":{"nodes":[{
                "number":1,"title":"Unknown","isDraft":false,
                "reviewDecision":"QUEUED",
                "repository":{"nameWithOwner":"owner/repo"}
            }],"pageInfo":{"hasNextPage":false,"endCursor":null}}}}}]"#,
        );
        assert!(global.is_err());
    }
}
