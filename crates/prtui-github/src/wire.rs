//! GitHub's JSON shapes and their conversion to application domain types.
//!
//! Raw JSON stops here. The rest of the crate exchanges typed requests and
//! domain models, so a schema change cannot leak into application state.

use anyhow::{Context, Result};
use prtui_core::{
    AddedThread, ChangedFile, Comment, DiffLine, LineKind, Meta, NewThread,
    Parent, PullRequest, ReviewEvent, ReviewThread, Side, parse_hunk_header,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

#[derive(Debug, Deserialize)]
struct File {
    filename: String,
    status: String,
    additions: u32,
    deletions: u32,
    patch: Option<String>,
}

impl From<File> for ChangedFile {
    fn from(file: File) -> Self {
        let lines = file.patch.as_deref().map(parse_patch).unwrap_or_default();

        Self {
            path: file.filename.into(),
            status: file.status,
            additions: file.additions,
            deletions: file.deletions,
            lines,
        }
    }
}

fn parse_patch(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::with_capacity(patch.lines().count());
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
            LineKind::Context | LineKind::Hunk => {
                (Some(old_line), Some(new_line))
            }
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

/// The path a `---`/`+++` header names, ignoring the side that is `/dev/null`.
fn header_path(line: &str) -> Option<&str> {
    line.strip_prefix("+++ b/")
        .or_else(|| line.strip_prefix("--- a/"))
}

fn take_patch(
    patches: &mut HashMap<String, Vec<DiffLine>>,
    path: Option<&str>,
    body: &mut String,
) {
    if let Some(path) = path
        && !body.is_empty()
    {
        patches.insert(path.to_string(), parse_patch(body));
    }

    body.clear();
}

/// Splits a whole-pull-request unified diff into a patch per wanted path.
///
/// A `diff --git` line can only be the next file's header, since every line of
/// a hunk carries a ` `, `+`, `-` or `\` prefix, and a `---` or `+++` line can
/// only name a path before the file's first hunk. A file nobody asked for is
/// skipped rather than parsed: the whole diff is fetched to recover the two or
/// three patches inside it that arrived empty.
pub fn split_diff(
    diff: &str,
    wanted: &HashSet<&str>,
) -> HashMap<String, Vec<DiffLine>> {
    let mut patches = HashMap::new();
    let mut path = None;
    let mut is_in_hunk = false;
    let mut body = String::new();

    for raw in diff.lines() {
        if raw.starts_with("diff --git ") {
            take_patch(&mut patches, path.take(), &mut body);
            is_in_hunk = false;
            continue;
        }

        if !is_in_hunk && !raw.starts_with("@@") {
            path = header_path(raw)
                .filter(|named| wanted.contains(named))
                .or(path);
            continue;
        }

        is_in_hunk = true;

        if path.is_some() {
            body.push_str(raw);
            body.push('\n');
        }
    }

    take_patch(&mut patches, path, &mut body);

    patches
}

fn convert_files(files: Vec<File>) -> Vec<ChangedFile> {
    files.into_iter().map(Into::into).collect()
}

/// One REST page, used by the paginated client.
pub fn file_page(bytes: &[u8]) -> Result<Vec<ChangedFile>> {
    let files = serde_json::from_slice(bytes)
        .context("unexpected /files page shape")?;
    Ok(convert_files(files))
}

/// A slurped list of REST pages, used by fixtures and other byte sources.
pub fn files(bytes: &[u8]) -> Result<Vec<ChangedFile>> {
    let pages: Vec<Vec<File>> = serde_json::from_slice(bytes)
        .context("expected array of pages from /files")?;
    let capacity = pages.iter().map(Vec::len).sum();
    let mut files = Vec::with_capacity(capacity);
    files.extend(pages.into_iter().flatten().map(Into::into));
    Ok(files)
}

/// The GraphQL response, shaped exactly as GitHub sends it.
///
/// Required fields deliberately have no defaults: a schema change is a parse
/// failure, not a plausible-looking empty value in the review.
#[derive(Deserialize)]
struct Response {
    data: ResponseData,
}

#[derive(Deserialize)]
struct ResponseData {
    repository: Option<Repository>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Repository {
    pull_request: Option<WirePullRequest>,
}

#[derive(Deserialize)]
struct Nodes<T> {
    nodes: Vec<T>,
}

#[derive(Deserialize)]
struct Author {
    login: String,
}

impl Author {
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
    author: Option<Author>,
    base_ref_name: String,
    head_ref_name: String,
    head_ref_oid: String,
    body: String,
    files: Nodes<WireChangedFile>,
    pending_review: Nodes<WireReview>,
    review_threads: Nodes<WireThread>,
    discussion: Nodes<WireDiscussionComment>,
}

#[derive(Deserialize)]
struct WireReview {
    id: String,
}

/// `DISMISSED` is a file marked read whose diff changed since.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WireChangedFile {
    path: String,
    viewer_viewed_state: String,
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
    author: Option<Author>,
    body: String,
    created_at: String,
}

/// `fullDatabaseId` is a `BigInt`, serialized as a string by GitHub. Older
/// deployments return a JSON number for the same field.
#[derive(Deserialize)]
#[serde(untagged)]
enum RestId {
    Text(String),
    Number(u64),
}

impl RestId {
    fn value(self) -> Arc<str> {
        match self {
            Self::Text(text) => text.into(),
            Self::Number(id) => id.to_string().into(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WireDiscussionComment {
    id: String,
    full_database_id: Option<RestId>,
    author: Option<Author>,
    body: String,
    created_at: String,
}

impl From<WireDiscussionComment> for Comment {
    fn from(comment: WireDiscussionComment) -> Self {
        Self {
            id: comment.id.into(),
            reply_target: comment.full_database_id.map(RestId::value),
            author: Author::login(comment.author),
            body: comment.body,
            created_at: comment.created_at,
            is_pending: false,
        }
    }
}

impl From<WireComment> for Comment {
    fn from(comment: WireComment) -> Self {
        Self {
            id: comment.id.into(),
            reply_target: comment.full_database_id.map(RestId::value),
            author: Author::login(comment.author),
            body: comment.body,
            created_at: comment.created_at,
            is_pending: comment.state == "PENDING",
        }
    }
}

impl From<WireThread> for ReviewThread {
    fn from(thread: WireThread) -> Self {
        let parse_side = |side: &str| match side {
            "LEFT" => Some(Side::Left),
            "RIGHT" => Some(Side::Right),
            _ => None,
        };

        Self {
            id: thread.id.into(),
            path: thread.path.into(),
            line: thread.line,
            original_line: thread.original_line,
            start_line: thread.start_line,
            // Unknown values degrade to the new side, where every line except
            // a deletion lives, instead of making the entire review unusable.
            side: parse_side(&thread.diff_side).unwrap_or(Side::Right),
            start_side: thread.start_diff_side.as_deref().and_then(parse_side),
            is_file_level: thread.subject_type == "FILE",
            is_resolved: thread.is_resolved,
            is_outdated: thread.is_outdated,
            can_resolve: thread.viewer_can_resolve,
            comments: thread
                .comments
                .nodes
                .into_iter()
                .map(Into::into)
                .collect(),
        }
    }
}

fn into_meta(response: Response) -> Result<Meta> {
    let pr = response
        .data
        .repository
        .and_then(|repository| repository.pull_request)
        .context("PR not found in graphql response")?;

    Ok(Meta {
        viewed: pr
            .files
            .nodes
            .into_iter()
            .filter(|file| file.viewer_viewed_state == "VIEWED")
            .map(|file| file.path.into())
            .collect(),
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
            author: Author::login(pr.author),
            base_ref: pr.base_ref_name,
            head_ref: pr.head_ref_name,
            head_oid: pr.head_ref_oid.into(),
            body: pr.body,
        },
        threads: pr
            .review_threads
            .nodes
            .into_iter()
            .map(Into::into)
            .collect(),
        discussion: pr.discussion.nodes.into_iter().map(Into::into).collect(),
    })
}

pub fn meta(value: Value) -> Result<Meta> {
    let response =
        serde_json::from_value(value).context("unexpected graphql response")?;
    into_meta(response)
}

pub fn meta_bytes(bytes: &[u8]) -> Result<Meta> {
    let response =
        serde_json::from_slice(bytes).context("unexpected graphql response")?;
    into_meta(response)
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

pub fn added_thread(value: Value) -> Result<AddedThread> {
    let payload: AddThreadResponse = serde_json::from_value(value)
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

const fn side(side: Side) -> &'static str {
    match side {
        Side::Left => "LEFT",
        Side::Right => "RIGHT",
    }
}

/// Serializes one typed draft at the last possible moment.
pub fn thread_variables(thread: NewThread) -> Value {
    let mut input = match thread.parent {
        Parent::Review(id) => json!({ "pullRequestReviewId": &*id }),
        Parent::PullRequest(id) => json!({ "pullRequestId": &*id }),
    };

    input["path"] = thread.path.to_string().into();
    input["body"] = thread.body.into();

    let Some(anchor) = thread.anchor else {
        input["subjectType"] = "FILE".into();
        return json!({ "input": input });
    };

    input["subjectType"] = "LINE".into();
    input["line"] = anchor.end_line.into();
    input["side"] = side(anchor.side).into();

    if anchor.is_multiline() {
        input["startLine"] = anchor.start_line.into();
        input["startSide"] = side(anchor.start_side).into();
    }

    json!({ "input": input })
}

const fn event_name(event: ReviewEvent) -> &'static str {
    match event {
        ReviewEvent::Comment => "COMMENT",
        ReviewEvent::Approve => "APPROVE",
        ReviewEvent::RequestChanges => "REQUEST_CHANGES",
    }
}

/// Serializes either kind of review target without making application state
/// carry GraphQL field names.
pub fn review_variables(
    parent: Parent,
    event: ReviewEvent,
    body: String,
) -> Value {
    let mut input = match parent {
        Parent::Review(id) => json!({ "pullRequestReviewId": &*id }),
        Parent::PullRequest(id) => json!({ "pullRequestId": &*id }),
    };

    input["event"] = event_name(event).into();
    if !body.is_empty() {
        input["body"] = body.into();
    }

    json!({ "input": input })
}

#[cfg(test)]
mod tests {
    use super::*;
    use prtui_core::Anchor;

    fn response(pull_request: &Value) -> Value {
        json!({ "data": { "repository": { "pullRequest": pull_request } } })
    }

    fn pull_request(threads: &Value) -> Value {
        json!({
            "id": "PR_1",
            "number": 9000,
            "title": "A change",
            "state": "OPEN",
            "isDraft": false,
            "author": { "login": "tale" },
            "baseRefName": "trunk",
            "headRefName": "work",
            "headRefOid": "cafe1234",
            "body": "",
            "files": { "nodes": [] },
            "pendingReview": { "nodes": [] },
            "reviewThreads": { "nodes": threads },
            "discussion": { "nodes": [] },
        })
    }

    #[test]
    fn a_line_thread_is_serialized_only_at_the_wire_boundary() {
        let variables = thread_variables(NewThread {
            parent: Parent::Review("PRR_1".into()),
            path: "src/main.rs".into(),
            body: "rewrite this".into(),
            anchor: Some(Anchor {
                start_line: 2,
                start_side: Side::Left,
                end_line: 4,
                side: Side::Right,
            }),
        });
        let input = &variables["input"];

        assert_eq!(input["pullRequestReviewId"], "PRR_1");
        assert_eq!(input["path"], "src/main.rs");
        assert_eq!(input["body"], "rewrite this");
        assert_eq!(input["subjectType"], "LINE");
        assert_eq!(input["startLine"], 2);
        assert_eq!(input["startSide"], "LEFT");
        assert_eq!(input["line"], 4);
        assert_eq!(input["side"], "RIGHT");
    }

    #[test]
    fn a_file_thread_has_no_line_fields() {
        let variables = thread_variables(NewThread {
            parent: Parent::PullRequest("PR_1".into()),
            path: "README.md".into(),
            body: "whole-file note".into(),
            anchor: None,
        });
        let input = &variables["input"];

        assert_eq!(input["pullRequestId"], "PR_1");
        assert_eq!(input["subjectType"], "FILE");
        assert!(input.get("line").is_none());
        assert!(input.get("side").is_none());
    }

    /// The diff endpoint is only reached for the files `/files` left empty, so
    /// what it hands back has to survive being cut apart by path — including a
    /// hunk line that looks like a header, and the `/dev/null` side of a file
    /// that was added or deleted.
    #[test]
    fn a_full_diff_yields_a_patch_for_each_file_asked_for() {
        let diff = "\
diff --git a/src/skipped.rs b/src/skipped.rs
index 1111111..2222222 100644
--- a/src/skipped.rs
+++ b/src/skipped.rs
@@ -1,2 +1,2 @@
-gone
+here
diff --git a/src/huge.rs b/src/huge.rs
index 3333333..4444444 100644
--- a/src/huge.rs
+++ b/src/huge.rs
@@ -10,3 +10,3 @@ fn context() {
 kept
--- a/not/a/header
+++ b/not/a/header
diff --git a/src/added.rs b/src/added.rs
new file mode 100644
index 0000000..5555555
--- /dev/null
+++ b/src/added.rs
@@ -0,0 +1,1 @@
+fresh
diff --git a/src/gone.rs b/src/gone.rs
deleted file mode 100644
index 6666666..0000000
--- a/src/gone.rs
+++ /dev/null
@@ -1,1 +0,0 @@
-old
";
        let wanted =
            HashSet::from(["src/huge.rs", "src/added.rs", "src/gone.rs"]);
        let patches = split_diff(diff, &wanted);

        assert_eq!(patches.len(), 3);
        assert!(!patches.contains_key("src/skipped.rs"));

        let huge = &patches["src/huge.rs"];
        assert_eq!(huge.len(), 4);
        assert_eq!(huge[0].kind, LineKind::Hunk);
        assert_eq!(huge[1].text, "kept");
        assert_eq!(huge[1].new_line, Some(10));
        // The one that reads as a header: a removed line whose own text starts
        // with `--`, and the added line under it.
        assert_eq!(huge[2].kind, LineKind::Removed);
        assert_eq!(huge[2].text, "-- a/not/a/header");
        assert_eq!(huge[3].kind, LineKind::Added);
        assert_eq!(huge[3].text, "++ b/not/a/header");

        assert_eq!(patches["src/added.rs"][1].text, "fresh");
        assert_eq!(patches["src/gone.rs"][1].kind, LineKind::Removed);
        assert_eq!(patches["src/gone.rs"][1].old_line, Some(1));
    }

    #[test]
    fn only_a_file_with_changes_and_no_lines_is_a_withheld_patch() {
        let file = |additions, deletions, lines: Vec<DiffLine>| ChangedFile {
            path: "src/state.zig".into(),
            status: "modified".into(),
            additions,
            deletions,
            lines,
        };
        let line = DiffLine {
            kind: LineKind::Added,
            text: "x".into(),
            old_line: None,
            new_line: Some(1),
        };

        assert!(file(6187, 0, vec![]).is_patch_withheld());
        assert!(file(0, 551, vec![]).is_patch_withheld());
        // A binary, mode-only or pure-rename change: nothing was withheld.
        assert!(!file(0, 0, vec![]).is_patch_withheld());
        assert!(!file(1, 0, vec![line]).is_patch_withheld());
    }

    #[test]
    fn a_bare_approval_omits_the_body() {
        let variables = review_variables(
            Parent::PullRequest("PR_1".into()),
            ReviewEvent::Approve,
            String::new(),
        );
        let input = &variables["input"];

        assert_eq!(input["pullRequestId"], "PR_1");
        assert_eq!(input["event"], "APPROVE");
        assert!(input.get("body").is_none());
    }

    #[test]
    fn a_field_the_api_stops_sending_fails_the_parse() {
        let mut pr = pull_request(&json!([]));
        pr.as_object_mut().unwrap().remove("state");

        assert!(meta(response(&pr)).is_err());
        assert!(meta(json!({ "data": {} })).is_err());
    }

    #[test]
    fn a_repository_that_is_not_there_says_so() {
        let empty = json!({ "data": { "repository": null } });
        let error = meta(empty).unwrap_err().to_string();

        assert!(error.contains("PR not found"), "{error}");
    }

    #[test]
    fn nullable_schema_fields_still_parse() {
        let threads = json!([{
            "id": "PRRT_1",
            "path": "src/main.rs",
            "subjectType": "LINE",
            "line": null,
            "originalLine": 12,
            "startLine": null,
            "diffSide": "LEFT",
            "startDiffSide": null,
            "isResolved": false,
            "isOutdated": true,
            "viewerCanResolve": true,
            "comments": { "nodes": [
                { "id": "PRRC_1", "state": "SUBMITTED", "fullDatabaseId": "1234", "author": null, "body": "hi", "createdAt": "now" },
                { "id": "PRRC_2", "state": "SUBMITTED", "fullDatabaseId": 5678, "author": { "login": "tale" }, "body": "ho", "createdAt": "now" },
            ] },
        }]);

        let parsed = meta(response(&pull_request(&threads))).unwrap();
        let thread = &parsed.threads[0];

        assert_eq!(thread.side, Side::Left);
        assert_eq!(thread.start_side, None);
        assert_eq!(thread.anchor_line(), Some(12));
        assert_eq!(thread.comments[0].author, "");
        assert!(!thread.is_pending());
        assert_eq!(thread.reply_target().as_deref(), Some("1234"));
        assert_eq!(thread.comments[1].reply_target.as_deref(), Some("5678"));
    }

    #[test]
    fn a_pending_thread_names_itself_and_its_review() {
        let threads = json!([{
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
        pr["pendingReview"] = json!({ "nodes": [{ "id": "PRR_1" }] });

        let parsed = meta(response(&pr)).unwrap();

        assert_eq!(parsed.pending_review.as_deref(), Some("PRR_1"));
        assert!(parsed.threads[0].is_pending());
        assert!(parsed.threads[0].is_file_level);
        assert_eq!(&*parsed.threads[0].comments[0].id, "PRRC_3");
    }
}
