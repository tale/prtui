use anyhow::{Context, Result};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    Hunk,
}

/// Which side of the diff a review thread is anchored to, matching GitHub's
/// `PullRequestReviewThreadDiffSide` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
}

impl Side {
    pub fn from_api(value: &str) -> Option<Self> {
        match value {
            "LEFT" => Some(Self::Left),
            "RIGHT" => Some(Self::Right),
            _ => None,
        }
    }

    pub const fn as_api(self) -> &'static str {
        match self {
            Self::Left => "LEFT",
            Self::Right => "RIGHT",
        }
    }
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    /// Shared: a file's path is its identity, and the threads, drafts and
    /// syntax colors filed against it all hold the same one.
    pub path: Arc<str>,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    filename: String,
    status: String,
    additions: u32,
    deletions: u32,
    patch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Comment {
    /// The GraphQL node id, which is what editing and discarding a draft are
    /// addressed to.
    pub id: Arc<str>,
    /// The REST id, which is what a reply has to be addressed to. GraphQL node
    /// ids are not interchangeable with it.
    pub rest_id: Option<u64>,
    pub author: String,
    pub body: String,
    pub created_at: String,
    /// A comment nobody but its author can see yet, because the review holding
    /// it has not been submitted.
    pub is_pending: bool,
}

#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each one mirrors an independent field of GitHub's thread"
)]
pub struct ReviewThread {
    pub id: Arc<str>,
    pub path: Arc<str>,
    pub line: Option<u32>,
    pub original_line: Option<u32>,
    pub start_line: Option<u32>,
    pub side: Side,
    /// Null for a thread that covers one line, where the start side is the
    /// only side there is.
    pub start_side: Option<Side>,
    /// A remark on the file rather than on any line in it.
    pub is_file_level: bool,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub can_resolve: bool,
    pub comments: Vec<Comment>,
}

impl ReviewThread {
    /// A thread is pending exactly when its first comment is, since a reply
    /// cannot be filed against a review that has not been submitted.
    pub fn is_pending(&self) -> bool {
        self.comments.first().is_some_and(|first| first.is_pending)
    }

    /// Current threads use `line`; GitHub clears it when a thread becomes
    /// outdated, leaving `originalLine` as the only usable display anchor.
    pub fn anchor_line(&self) -> Option<u32> {
        self.line.or(self.original_line)
    }

    /// Replies address the thread's first comment, which is the one GitHub
    /// treats as the conversation root.
    pub fn reply_target(&self) -> Option<u64> {
        self.comments.first().and_then(|comment| comment.rest_id)
    }

    pub fn anchors_to(&self, line: &DiffLine) -> bool {
        let Some(anchor) = self.anchor_line() else {
            return false;
        };
        match self.side {
            Side::Left => line.old_line == Some(anchor),
            Side::Right => line.new_line == Some(anchor),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PullRequest {
    /// The GraphQL node id, which is what a new draft names when no pending
    /// review exists yet to hang it on.
    pub id: Arc<str>,
    pub number: u32,
    pub title: String,
    pub state: String,
    pub is_draft: bool,
    pub author: String,
    pub base_ref: String,
    pub head_ref: String,
    pub body: String,
}

/// What one metadata fetch yields. The threads travel beside the pull request
/// rather than inside it: the app files them by path and nothing reads them in
/// the order they arrived.
#[derive(Debug, Clone)]
pub struct Meta {
    pub pr: PullRequest,
    pub threads: Vec<ReviewThread>,
    /// The review the viewer has open but not submitted, which every draft is
    /// filed against once the first one has opened it.
    pub pending_review: Option<Arc<str>>,
}

/// `@@ -old,count +new,count @@` — captures the two start line numbers.
fn parse_hunk_header(header: &str) -> Option<(u32, u32)> {
    let inner = header.strip_prefix("@@ ")?.split(" @@").next()?;
    let (old, new) = inner.split_once(' ')?;

    let start = |s: &str| -> Option<u32> {
        s.get(1..)?.split(',').next()?.parse().ok()
    };

    Some((start(old)?, start(new)?))
}

fn parse_patch(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut old_line = 0;
    let mut new_line = 0;

    for raw in patch.lines() {
        if raw.starts_with("@@") {
            let Some((old, new)) = parse_hunk_header(raw) else {
                continue;
            };

            old_line = old;
            new_line = new;
            lines.push(DiffLine {
                kind: LineKind::Hunk,
                text: raw.to_string(),
                old_line: None,
                new_line: None,
            });
            continue;
        }

        let (kind, text) = match raw.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &raw[1..]),
            Some(b'-') => (LineKind::Removed, &raw[1..]),
            Some(b' ') => (LineKind::Context, &raw[1..]),
            _ => continue,
        };

        let (old, new) = match kind {
            LineKind::Added => (None, Some(new_line)),
            LineKind::Removed => (Some(old_line), None),
            _ => (Some(old_line), Some(new_line)),
        };

        if kind != LineKind::Added {
            old_line += 1;
        }
        if kind != LineKind::Removed {
            new_line += 1;
        }

        lines.push(DiffLine {
            kind,
            text: text.to_string(),
            old_line: old,
            new_line: new,
        });
    }

    lines
}

/// `gh api --paginate --slurp` returns an array of pages, each an array of files.
pub fn parse_files(val: &serde_json::Value) -> Result<Vec<ChangedFile>> {
    let pages = val
        .as_array()
        .context("expected array of pages from /files")?;

    let mut files = Vec::new();
    for page in pages {
        let raws: Vec<RawFile> = serde_json::from_value(page.clone())
            .context("unexpected /files page shape")?;

        for raw in raws {
            let lines =
                raw.patch.as_deref().map(parse_patch).unwrap_or_default();

            files.push(ChangedFile {
                path: raw.filename.into(),
                status: raw.status,
                additions: raw.additions,
                deletions: raw.deletions,
                lines,
            });
        }
    }

    Ok(files)
}

/// The GraphQL response, shaped exactly as GitHub sends it.
///
/// Everything below is `#[serde]` rather than hand-walked, so a field the API
/// stops sending fails the parse instead of silently defaulting to zero or an
/// empty string. Only what the schema declares nullable is `Option` here.
#[derive(Deserialize)]
struct Response {
    data: ResponseData,
}

#[derive(Deserialize)]
struct ResponseData {
    repository: Option<WireRepository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireRepository {
    pull_request: Option<WirePullRequest>,
}

/// A GraphQL connection, of which only the nodes are ever wanted.
#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct WireAuthor {
    login: String,
}

impl WireAuthor {
    /// An account that has since been deleted comes back as a null author.
    fn login(author: Option<Self>) -> String {
        author.map(|author| author.login).unwrap_or_default()
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WirePullRequest {
    id: String,
    number: u32,
    title: String,
    state: String,
    is_draft: bool,
    author: Option<WireAuthor>,
    base_ref_name: String,
    head_ref_name: String,
    body: String,
    pending_review: Nodes<WireReview>,
    review_threads: Nodes<WireThread>,
}

#[derive(Deserialize)]
struct WireReview {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireThread {
    id: String,
    path: String,
    subject_type: String,
    line: Option<u32>,
    original_line: Option<u32>,
    start_line: Option<u32>,
    diff_side: String,
    start_diff_side: Option<String>,
    is_resolved: bool,
    is_outdated: bool,
    viewer_can_resolve: bool,
    comments: Nodes<WireComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireComment {
    id: String,
    full_database_id: Option<RestId>,
    state: String,
    author: Option<WireAuthor>,
    body: String,
    created_at: String,
}

/// `fullDatabaseId` is a `BigInt`, which GitHub serializes as a string; older
/// deployments hand back a plain number for the same field.
#[derive(Deserialize)]
#[serde(untagged)]
enum RestId {
    Text(String),
    Number(u64),
}

impl RestId {
    fn value(self) -> Option<u64> {
        match self {
            Self::Text(text) => text.parse().ok(),
            Self::Number(id) => Some(id),
        }
    }
}

impl From<WireComment> for Comment {
    fn from(wire: WireComment) -> Self {
        Self {
            id: wire.id.into(),
            rest_id: wire.full_database_id.and_then(RestId::value),
            author: WireAuthor::login(wire.author),
            body: wire.body,
            created_at: wire.created_at,
            is_pending: wire.state == "PENDING",
        }
    }
}

impl From<WireThread> for ReviewThread {
    fn from(wire: WireThread) -> Self {
        Self {
            id: wire.id.into(),
            path: wire.path.into(),
            line: wire.line,
            original_line: wire.original_line,
            start_line: wire.start_line,
            // A side the app does not recognize is treated as the new file,
            // which is where all but deletions live.
            side: Side::from_api(&wire.diff_side).unwrap_or(Side::Right),
            start_side: wire
                .start_diff_side
                .as_deref()
                .and_then(Side::from_api),
            is_file_level: wire.subject_type == "FILE",
            is_resolved: wire.is_resolved,
            is_outdated: wire.is_outdated,
            can_resolve: wire.viewer_can_resolve,
            comments: wire.comments.nodes.into_iter().map(Into::into).collect(),
        }
    }
}

pub fn parse_meta(val: &serde_json::Value) -> Result<Meta> {
    let response =
        Response::deserialize(val).context("unexpected graphql response")?;
    let pr = response
        .data
        .repository
        .and_then(|repository| repository.pull_request)
        .context("PR not found in graphql response")?;

    Ok(Meta {
        pending_review: pr
            .pending_review
            .nodes
            .into_iter()
            .next()
            .map(|review| review.id.into()),
        pr: PullRequest {
            id: pr.id.into(),
            number: pr.number,
            title: pr.title,
            state: pr.state,
            is_draft: pr.is_draft,
            author: WireAuthor::login(pr.author),
            base_ref: pr.base_ref_name,
            head_ref: pr.head_ref_name,
            body: pr.body,
        },
        threads: pr
            .review_threads
            .nodes
            .into_iter()
            .map(Into::into)
            .collect(),
    })
}

/// What filing a draft comment hands back: the comment to address later edits
/// to, and the review it was filed against, which the next draft reuses.
#[derive(Debug, Clone)]
pub struct AddedThread {
    pub review: Arc<str>,
    pub comment: Arc<str>,
}

#[derive(Deserialize)]
struct AddThreadResponse {
    data: AddThreadData,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct AddThreadData {
    add_pull_request_review_thread: AddThreadPayload,
}

#[derive(Deserialize)]
struct AddThreadPayload {
    thread: WireAddedThread,
}

#[derive(Deserialize)]
struct WireAddedThread {
    comments: Nodes<WireAddedComment>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireAddedComment {
    id: String,
    pull_request_review: WireReview,
}

pub fn parse_added_thread(val: &serde_json::Value) -> Result<AddedThread> {
    let payload = AddThreadResponse::deserialize(val)
        .context("unexpected addPullRequestReviewThread response")?;
    let comment = payload
        .data
        .add_pull_request_review_thread
        .thread
        .comments
        .nodes
        .into_iter()
        .next()
        .context("draft came back with no comment")?;

    Ok(AddedThread {
        review: comment.pull_request_review.id.into(),
        comment: comment.id.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(pull_request: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "data": { "repository": { "pullRequest": pull_request } }
        })
    }

    fn pull_request(threads: &serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "id": "PR_1",
            "number": 9000,
            "title": "A change",
            "state": "OPEN",
            "isDraft": false,
            "author": { "login": "tale" },
            "baseRefName": "trunk",
            "headRefName": "work",
            "body": "",
            "pendingReview": { "nodes": [] },
            "reviewThreads": { "nodes": threads },
        })
    }

    /// The hand-walked parser defaulted every field it could not find, so a
    /// renamed one showed up as an empty title rather than as an error.
    #[test]
    fn a_field_the_api_stops_sending_fails_the_parse() {
        let mut pr = pull_request(&serde_json::json!([]));
        pr.as_object_mut().unwrap().remove("state");

        assert!(parse_meta(&response(&pr)).is_err());
        assert!(parse_meta(&serde_json::json!({ "data": {} })).is_err());
    }

    #[test]
    fn a_repository_that_is_not_there_says_so() {
        let empty = serde_json::json!({ "data": { "repository": null } });
        let error = parse_meta(&empty).unwrap_err().to_string();

        assert!(error.contains("PR not found"), "{error}");
    }

    #[test]
    fn what_the_schema_allows_to_be_null_still_parses() {
        let threads = serde_json::json!([{
            "id": "PRRT_1",
            "path": "src/main.rs",
            "subjectType": "LINE",
            // An outdated thread loses its line, and a deleted account its login.
            "line": null,
            "originalLine": 12,
            "startLine": null,
            "diffSide": "LEFT",
            // Null on a thread covering one line, where there is only one side.
            "startDiffSide": null,
            "isResolved": false,
            "isOutdated": true,
            "viewerCanResolve": true,
            "comments": { "nodes": [
                { "id": "PRRC_1", "state": "SUBMITTED", "fullDatabaseId": "1234", "author": null, "body": "hi", "createdAt": "now" },
                { "id": "PRRC_2", "state": "SUBMITTED", "fullDatabaseId": 5678, "author": { "login": "tale" }, "body": "ho", "createdAt": "now" },
            ] },
        }]);

        let meta = parse_meta(&response(&pull_request(&threads))).unwrap();
        let thread = &meta.threads[0];

        assert_eq!(thread.side, Side::Left);
        assert_eq!(thread.start_side, None);
        assert_eq!(thread.anchor_line(), Some(12));
        assert_eq!(thread.comments[0].author, "");
        assert!(!thread.is_pending());
        // A BigInt arrives as a string, but older deployments send a number.
        assert_eq!(thread.reply_target(), Some(1234));
        assert_eq!(thread.comments[1].rest_id, Some(5678));
    }

    /// An unsubmitted comment is this session's own draft. It comes back on
    /// every fetch, so telling it apart is what keeps it off the diff as a
    /// conversation.
    #[test]
    fn a_pending_thread_names_itself_and_its_review() {
        let threads = serde_json::json!([{
            "id": "PRRT_2",
            "path": "src/main.rs",
            "subjectType": "FILE",
            "line": null,
            "originalLine": null,
            "startLine": null,
            "diffSide": "RIGHT",
            "startDiffSide": null,
            "isResolved": false,
            "isOutdated": false,
            "viewerCanResolve": false,
            "comments": { "nodes": [
                { "id": "PRRC_3", "state": "PENDING", "fullDatabaseId": null, "author": { "login": "tale" }, "body": "wip", "createdAt": "now" },
            ] },
        }]);

        let mut pr = pull_request(&threads);
        pr["pendingReview"] =
            serde_json::json!({ "nodes": [{ "id": "PRR_1" }] });

        let meta = parse_meta(&response(&pr)).unwrap();

        assert_eq!(meta.pending_review.as_deref(), Some("PRR_1"));
        assert!(meta.threads[0].is_pending());
        assert!(meta.threads[0].is_file_level);
        assert_eq!(&*meta.threads[0].comments[0].id, "PRRC_3");
    }
}
