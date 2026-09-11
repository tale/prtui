//! Deserialization of GitLab responses and conversion into domain models.

use anyhow::{Context, Result, bail};
use prtui_core::{
    Anchor, ChangedFile, Check, CheckState, Comment, LineKind, PullRequest,
    ReviewEvent, ReviewThread, Reviewer, Side, Verdict, parse_hunk_header,
    parse_patch,
};
use serde::Deserialize;
use std::fmt::Write;
use std::sync::Arc;

use crate::ids::{CommentRef, ReplyRef, ThreadRef};

#[derive(Debug, Clone, Deserialize)]
pub struct WireUser {
    pub name: Option<String>,
    pub username: String,
}

impl WireUser {
    pub fn display(&self) -> String {
        self.name.clone().unwrap_or_else(|| self.username.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[allow(
    clippy::struct_field_names,
    reason = "the three shas are GitLab's own field names"
)]
pub struct DiffRefs {
    pub base_sha: String,
    pub start_sha: String,
    pub head_sha: String,
}

#[derive(Debug, Deserialize)]
pub struct WireMergeRequest {
    pub iid: u32,
    pub title: String,
    pub state: String,
    #[serde(default)]
    pub draft: bool,
    pub author: Option<WireUser>,
    pub source_branch: String,
    pub target_branch: String,
    pub sha: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
    pub updated_at: String,
    #[serde(default)]
    pub diff_refs: Option<DiffRefs>,
    #[serde(default)]
    pub reviewers: Vec<WireUser>,
    #[serde(default)]
    pub references: Option<WireReferences>,
}

#[derive(Debug, Deserialize)]
pub struct WireReferences {
    pub full: String,
}

impl WireMergeRequest {
    pub fn into_pull_request(self) -> PullRequest {
        PullRequest {
            id: Arc::from(self.iid.to_string()),
            number: self.iid,
            title: self.title,
            state: self.state,
            is_draft: self.draft,
            author: self
                .author
                .as_ref()
                .map(WireUser::display)
                .unwrap_or_default(),
            base_ref: self.target_branch,
            head_ref: self.source_branch,
            head_oid: Arc::from(self.sha.unwrap_or_default()),
            body: self.description.unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag is an independent fact GitLab reports about the file"
)]
pub struct WireDiff {
    pub old_path: String,
    pub new_path: String,
    pub diff: String,
    #[serde(default)]
    pub new_file: bool,
    #[serde(default)]
    pub renamed_file: bool,
    #[serde(default)]
    pub deleted_file: bool,
    #[serde(default)]
    pub too_large: bool,
}

impl WireDiff {
    pub fn to_changed_file(&self) -> ChangedFile {
        let lines = if self.too_large {
            Vec::new()
        } else {
            parse_patch(&self.diff)
        };

        let count = |kind: LineKind| {
            lines.iter().filter(|line| line.kind == kind).count() as u32
        };

        ChangedFile {
            path: Arc::from(self.new_path.as_str()),
            status: self.status().to_owned(),
            additions: count(LineKind::Added),
            deletions: count(LineKind::Removed),
            lines,
        }
    }

    const fn status(&self) -> &'static str {
        if self.new_file {
            return "added";
        }
        if self.deleted_file {
            return "removed";
        }
        if self.renamed_file {
            return "renamed";
        }

        "modified"
    }
}

#[derive(Debug, Deserialize)]
pub struct WireDiscussion {
    pub id: String,
    pub notes: Vec<WireNote>,
}

#[derive(Debug, Deserialize)]
pub struct WireNote {
    pub id: u64,
    pub body: String,
    pub author: WireUser,
    pub created_at: String,
    #[serde(default)]
    pub system: bool,
    #[serde(default)]
    pub resolvable: bool,
    #[serde(default)]
    pub resolved: Option<bool>,
    #[serde(default)]
    pub position: Option<WirePosition>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WirePosition {
    pub position_type: String,
    #[serde(default)]
    pub old_path: Option<String>,
    #[serde(default)]
    pub new_path: Option<String>,
    #[serde(default)]
    pub old_line: Option<u32>,
    #[serde(default)]
    pub new_line: Option<u32>,
    #[serde(default)]
    pub head_sha: Option<String>,
    #[serde(default)]
    pub line_range: Option<WireLineRange>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireLineRange {
    pub start: Option<WireLinePoint>,
    pub end: Option<WireLinePoint>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WireLinePoint {
    #[serde(rename = "type")]
    pub kind: Option<String>,
    #[serde(default)]
    pub old_line: Option<u32>,
    #[serde(default)]
    pub new_line: Option<u32>,
}

impl WireLinePoint {
    fn side(&self) -> Side {
        match self.kind.as_deref() {
            Some("old") => Side::Left,
            Some("new") => Side::Right,
            _ => self.new_line.map_or(Side::Left, |_| Side::Right),
        }
    }

    fn line(&self) -> Option<u32> {
        match self.side() {
            Side::Left => self.old_line,
            Side::Right => self.new_line,
        }
    }
}

impl WirePosition {
    fn side(&self) -> Side {
        self.line_range
            .as_ref()
            .and_then(|range| range.end.as_ref())
            .map_or_else(
                || self.new_line.map_or(Side::Left, |_| Side::Right),
                WireLinePoint::side,
            )
    }

    fn line(&self) -> Option<u32> {
        match self.side() {
            Side::Left => self.old_line,
            Side::Right => self.new_line,
        }
    }

    fn is_file_level(&self) -> bool {
        self.position_type == "file"
    }

    pub const fn is_anchored(&self) -> bool {
        self.new_path.is_some() || self.old_path.is_some()
    }
}

impl WireNote {
    fn into_comment(self, mr: u32, discussion: &str) -> Comment {
        let reply = ReplyRef {
            discussion: discussion.to_owned(),
            note: self.id,
        };

        Comment {
            id: Arc::from(
                CommentRef::Published { mr, note: self.id }.to_string(),
            ),
            reply_target: Some(Arc::from(reply.to_string())),
            author: self.author.display(),
            body: self.body,
            created_at: self.created_at,
            is_pending: false,
        }
    }
}

pub fn split_discussions(
    discussions: Vec<WireDiscussion>,
    mr: u32,
    head: Option<&str>,
) -> (Vec<ReviewThread>, Vec<Comment>) {
    let mut threads = Vec::new();
    let mut conversation = Vec::new();

    for discussion in discussions {
        let notes: Vec<WireNote> =
            discussion.notes.into_iter().filter(|n| !n.system).collect();
        let Some(first) = notes.first() else {
            continue;
        };

        let Some(position) = first.position.clone() else {
            conversation.extend(
                notes
                    .into_iter()
                    .map(|note| note.into_comment(mr, &discussion.id)),
            );
            continue;
        };

        threads.push(thread_from(&discussion.id, notes, &position, mr, head));
    }

    (threads, conversation)
}

fn start_of(position: &WirePosition) -> Option<(u32, Side)> {
    let start = position.line_range.as_ref()?.start.as_ref()?;
    Some((start.line()?, start.side()))
}

fn thread_from(
    discussion: &str,
    notes: Vec<WireNote>,
    position: &WirePosition,
    mr: u32,
    head: Option<&str>,
) -> ReviewThread {
    let first = &notes[0];
    let is_outdated = match (position.head_sha.as_deref(), head) {
        (Some(anchored), Some(current)) => anchored != current,
        _ => false,
    };
    let line = position.line();

    let start = start_of(position);

    let id = ThreadRef {
        mr,
        discussion: discussion.to_owned(),
    };

    ReviewThread {
        id: Arc::from(id.to_string()),
        path: Arc::from(
            position
                .new_path
                .as_deref()
                .or(position.old_path.as_deref())
                .unwrap_or_default(),
        ),
        line: (!is_outdated).then_some(line).flatten(),
        original_line: is_outdated.then_some(line).flatten(),
        start_line: start.map(|(line, _)| line),
        side: position.side(),
        start_side: start.map(|(_, side)| side),
        is_file_level: position.is_file_level(),
        is_resolved: first.resolved.unwrap_or(false),
        can_resolve: first.resolvable,
        is_outdated,
        comments: notes
            .into_iter()
            .map(|note| note.into_comment(mr, discussion))
            .collect(),
    }
}

#[derive(Debug, Deserialize)]
pub struct WireDraftNote {
    pub id: u64,
    pub note: String,
    #[serde(default)]
    pub position: Option<WirePosition>,
}

impl WireDraftNote {
    pub fn into_pending(self, mr: u32, author: &str) -> Pending {
        let comment = Comment {
            id: Arc::from(CommentRef::Draft { mr, draft: self.id }.to_string()),
            reply_target: None,
            author: author.to_owned(),
            body: self.note,
            created_at: String::new(),
            is_pending: true,
        };

        let Some(position) = self.position.filter(WirePosition::is_anchored)
        else {
            return Pending::Discussion(comment);
        };

        let start = start_of(&position);

        Pending::Thread(ReviewThread {
            id: Arc::from(format!("draft:{mr}:{}", comment.id)),
            path: Arc::from(
                position
                    .new_path
                    .as_deref()
                    .or(position.old_path.as_deref())
                    .unwrap_or_default(),
            ),
            line: position.line(),
            original_line: None,
            start_line: start.map(|(line, _)| line),
            side: position.side(),
            start_side: start.map(|(_, side)| side),
            is_file_level: position.is_file_level(),
            is_resolved: false,
            can_resolve: false,
            is_outdated: false,
            comments: vec![comment],
        })
    }
}

pub enum Pending {
    Thread(ReviewThread),
    Discussion(Comment),
}

#[derive(Debug, Deserialize)]
pub struct WireTreeEntry {
    pub id: String,
    pub path: String,
}

#[derive(Debug, Deserialize)]
pub struct WirePipeline {
    pub id: u64,
}

#[derive(Debug, Deserialize)]
pub struct WireJob {
    pub name: String,
    pub status: String,
}

impl WireJob {
    pub fn into_check(self) -> Check {
        Check {
            name: self.name,
            state: check_state(&self.status),
        }
    }
}

pub fn check_state(status: &str) -> CheckState {
    match status {
        "success" => CheckState::Passed,
        "failed" => CheckState::Failed,
        "canceled" | "canceling" | "skipped" | "manual" | "scheduled" => {
            CheckState::Skipped
        }
        _ => CheckState::Running,
    }
}

#[derive(Debug, Deserialize)]
pub struct WireApprovals {
    #[serde(default)]
    pub approved_by: Vec<WireApproval>,
}

#[derive(Debug, Deserialize)]
pub struct WireApproval {
    pub user: WireUser,
}

#[derive(Debug, Deserialize)]
pub struct WireReviewer {
    pub user: WireUser,
    pub state: String,
}

pub fn review_submission(event: ReviewEvent) -> serde_json::Value {
    match event {
        ReviewEvent::RequestChanges => {
            serde_json::json!({ "reviewer_state": "requested_changes" })
        }
        ReviewEvent::Comment => {
            serde_json::json!({ "reviewer_state": "reviewed" })
        }
        ReviewEvent::Approve => serde_json::json!({}),
    }
}

pub fn verify_requested_changes(
    reviewers: &[WireReviewer],
    username: &str,
) -> Result<()> {
    if reviewers.iter().any(|reviewer| {
        reviewer.user.username == username
            && reviewer.state == "requested_changes"
    }) {
        return Ok(());
    }

    bail!(
        "review comments were published, but GitLab did not record requested changes for {username}; check the server's review support and your reviewer permissions before retrying"
    )
}

pub fn reviewers(
    requested: &[WireReviewer],
    approvals: &WireApprovals,
) -> Vec<Reviewer> {
    let has_approved = |user: &WireUser| {
        approvals
            .approved_by
            .iter()
            .any(|approval| approval.user.username == user.username)
    };

    let mut reviewers: Vec<Reviewer> = requested
        .iter()
        .map(|reviewer| Reviewer {
            name: reviewer.user.display(),
            is_team: false,
            verdict: if reviewer.state == "requested_changes" {
                Verdict::ChangesRequested
            } else if has_approved(&reviewer.user) {
                Verdict::Approved
            } else if reviewer.state == "reviewed" {
                Verdict::Commented
            } else {
                Verdict::Waiting
            },
        })
        .collect();

    for approval in &approvals.approved_by {
        if requested
            .iter()
            .any(|reviewer| reviewer.user.username == approval.user.username)
        {
            continue;
        }

        reviewers.push(Reviewer {
            name: approval.user.display(),
            is_team: false,
            verdict: Verdict::Approved,
        });
    }

    reviewers
}

pub fn position(
    refs: &DiffRefs,
    file: &WireDiff,
    anchor: Option<Anchor>,
) -> Result<serde_json::Value> {
    let mut position = serde_json::json!({
        "base_sha": refs.base_sha,
        "start_sha": refs.start_sha,
        "head_sha": refs.head_sha,
        "old_path": file.old_path,
        "new_path": file.new_path,
        "position_type": "file",
    });
    let Some(anchor) = anchor else {
        return Ok(position);
    };
    if file.too_large || file.diff.is_empty() {
        bail!("cannot anchor a line comment without a complete text diff");
    }

    let end = diff_position(&file.diff, anchor.end_line, anchor.side)?;
    position["position_type"] = "text".into();
    if end.kind != LineKind::Added {
        position["old_line"] = end.old.into();
    }
    if end.kind != LineKind::Removed {
        position["new_line"] = end.new.into();
    }
    if !anchor.is_multiline() {
        return Ok(position);
    }

    let start =
        diff_position(&file.diff, anchor.start_line, anchor.start_side)?;
    position["line_range"] = serde_json::json!({
        "start": start.line_point(&file.new_path, anchor.start_side),
        "end": end.line_point(&file.new_path, anchor.side),
    });

    Ok(position)
}

struct DiffPosition {
    old: u32,
    new: u32,
    kind: LineKind,
}

impl DiffPosition {
    fn line_point(&self, path: &str, side: Side) -> serde_json::Value {
        let hash = ring::digest::digest(
            &ring::digest::SHA1_FOR_LEGACY_USE_ONLY,
            path.as_bytes(),
        );
        let mut hex = String::with_capacity(40);
        for byte in hash.as_ref() {
            let _ = write!(hex, "{byte:02x}");
        }

        serde_json::json!({
            "line_code": format!("{hex}_{}_{}", self.old, self.new),
            "type": if side == Side::Right { "new" } else { "old" },
            "old_line": (self.kind != LineKind::Added).then_some(self.old),
            "new_line": (self.kind != LineKind::Removed).then_some(self.new),
        })
    }
}

fn diff_position(patch: &str, line: u32, side: Side) -> Result<DiffPosition> {
    if line == 0 {
        bail!("diff line numbers must be positive");
    }

    let (mut old, mut new) = (1, 1);
    let side_line = |old, new| if side == Side::Left { old } else { new };
    for raw in patch.lines() {
        if raw.starts_with("@@") {
            let (next_old, next_new) =
                parse_hunk_header(raw).context("invalid diff hunk header")?;
            if line < side_line(next_old, next_new) {
                break;
            }
            (old, new) = (next_old, next_new);
            continue;
        }

        let kind = match raw.as_bytes().first() {
            Some(b'+') => LineKind::Added,
            Some(b'-') => LineKind::Removed,
            Some(b' ') => LineKind::Context,
            _ => continue,
        };
        let is_present = match side {
            Side::Left => kind != LineKind::Added,
            Side::Right => kind != LineKind::Removed,
        };
        if is_present && line == side_line(old, new) {
            return Ok(DiffPosition { old, new, kind });
        }
        old += u32::from(kind != LineKind::Added);
        new += u32::from(kind != LineKind::Removed);
    }

    // Expanded context is absent from the patch but retains the preceding offset.
    let offset = line
        .checked_sub(side_line(old, new))
        .context("line is not present on the selected side of the diff")?;
    Ok(DiffPosition {
        old: old.checked_add(offset).context("diff line overflow")?,
        new: new.checked_add(offset).context("diff line overflow")?,
        kind: LineKind::Context,
    })
}

pub fn changed_files(diffs: &[WireDiff]) -> Vec<ChangedFile> {
    diffs.iter().map(WireDiff::to_changed_file).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs() -> DiffRefs {
        DiffRefs {
            base_sha: "base".to_owned(),
            start_sha: "start".to_owned(),
            head_sha: "head".to_owned(),
        }
    }

    fn renamed_diff() -> WireDiff {
        serde_json::from_value(serde_json::json!({
            "old_path": "old.rs", "new_path": "new.rs", "renamed_file": true,
            "diff": "@@ -3,3 +3,4 @@\n before\n-old\n+new\n+extra\n after\n"
        }))
        .unwrap()
    }

    #[test]
    fn context_comments_preserve_both_paths_and_shifted_line_numbers() {
        let position = position(
            &refs(),
            &renamed_diff(),
            Some(Anchor::spanning(6, 6, Side::Right)),
        )
        .unwrap();
        assert_eq!(position["old_path"], "old.rs");
        assert_eq!(position["new_path"], "new.rs");
        assert_eq!(position["old_line"], 5);
        assert_eq!(position["new_line"], 6);
    }

    #[test]
    fn changed_lines_only_send_the_side_that_exists() {
        let file = renamed_diff();
        let added =
            position(&refs(), &file, Some(Anchor::spanning(4, 4, Side::Right)))
                .unwrap();
        assert_eq!(added["new_line"], 4);
        assert!(added.get("old_line").is_none());
        let removed =
            position(&refs(), &file, Some(Anchor::spanning(4, 4, Side::Left)))
                .unwrap();
        assert_eq!(removed["old_line"], 4);
        assert!(removed.get("new_line").is_none());
    }

    #[test]
    fn multiline_comments_include_line_codes_and_keep_cross_side_ranges() {
        let anchor = Anchor {
            start_line: 4,
            start_side: Side::Left,
            end_line: 5,
            side: Side::Right,
        };
        let position =
            position(&refs(), &renamed_diff(), Some(anchor)).unwrap();
        assert_eq!(
            position["line_range"],
            serde_json::json!({
                "start": { "type": "old", "old_line": 4, "new_line": null, "line_code": "e6d049e7e635307aff069ed13eadd2e56f36bc52_4_4" },
                "end": { "type": "new", "old_line": null, "new_line": 5, "line_code": "e6d049e7e635307aff069ed13eadd2e56f36bc52_5_5" }
            })
        );
    }

    #[test]
    fn expanded_context_uses_the_offset_before_or_after_a_hunk() {
        for (old, new) in [(1, 1), (10, 11)] {
            let position = position(
                &refs(),
                &renamed_diff(),
                Some(Anchor::spanning(new, new, Side::Right)),
            )
            .unwrap();
            assert_eq!(position["old_line"], old);
            assert_eq!(position["new_line"], new);
        }
    }

    #[test]
    fn file_comments_need_no_line_position_but_withheld_line_comments_fail() {
        let mut file = renamed_diff();
        file.diff.clear();
        file.too_large = true;
        let position = position(&refs(), &file, None).unwrap();
        assert_eq!(position["position_type"], "file");
        assert_eq!(position["old_path"], "old.rs");
        assert!(position.get("new_line").is_none());
        assert!(
            super::position(
                &refs(),
                &file,
                Some(Anchor::spanning(1, 1, Side::Right))
            )
            .is_err()
        );
    }
    #[test]
    fn saved_multiline_context_uses_the_explicit_side_for_each_endpoint() {
        let position = serde_json::json!({
            "position_type": "text", "old_path": "cases/context.txt", "new_path": "cases/context.txt",
            "old_line": 49, "new_line": 48,
            "line_range": {
                "start": { "line_code": "42df38935f2209660c8354509475b96d137ab093_48_47", "old_line": 48, "new_line": 47, "type": "old" },
                "end": { "old_line": 49, "new_line": 48, "type": "old" }
            }
        });
        let draft: WireDraftNote = serde_json::from_value(serde_json::json!({
            "id": 47, "note": "context range", "position": position
        }))
        .unwrap();
        let Pending::Thread(thread) = draft.into_pending(2, "Reviewer") else {
            panic!("expected a diff thread")
        };
        assert_eq!(
            (thread.start_line, thread.start_side),
            (Some(48), Some(Side::Left))
        );
        assert_eq!((thread.line, thread.side), (Some(49), Side::Left));

        let discussion: WireDiscussion = serde_json::from_value(serde_json::json!({
            "id": "discussion", "notes": [{ "id": 1, "body": "context range", "created_at": "2026-09-11T00:00:00Z", "author": { "username": "reviewer" }, "position": position }]
        })).unwrap();
        let (threads, _) = split_discussions(vec![discussion], 2, None);
        assert_eq!(
            (threads[0].start_line, threads[0].start_side),
            (Some(48), Some(Side::Left))
        );
        assert_eq!((threads[0].line, threads[0].side), (Some(49), Side::Left));
    }
    #[test]
    fn review_submission_sends_the_verdict_without_faking_an_approval() {
        assert_eq!(
            review_submission(ReviewEvent::RequestChanges),
            serde_json::json!({ "reviewer_state": "requested_changes" })
        );
        assert_eq!(
            review_submission(ReviewEvent::Comment),
            serde_json::json!({ "reviewer_state": "reviewed" })
        );
        assert_eq!(
            review_submission(ReviewEvent::Approve),
            serde_json::json!({})
        );
    }

    #[test]
    fn requested_changes_must_belong_to_the_current_reviewer() {
        let reviewers: Vec<WireReviewer> = serde_json::from_value(serde_json::json!([
            { "user": { "username": "alice" }, "state": "requested_changes" },
            { "user": { "username": "bob" }, "state": "reviewed" }
        ])).unwrap();
        assert!(verify_requested_changes(&reviewers, "alice").is_ok());
        for username in ["bob", "unassigned"] {
            let error =
                verify_requested_changes(&reviewers, username).unwrap_err();
            assert!(error.to_string().contains("comments were published"));
        }
        assert!(verify_requested_changes(&[], "alice").is_err());
    }

    #[test]
    fn reviewer_summaries_distinguish_changes_comments_and_approvals() {
        let requested: Vec<WireReviewer> = serde_json::from_value(serde_json::json!([
            { "user": { "username": "alice" }, "state": "requested_changes" },
            { "user": { "username": "bob" }, "state": "reviewed" },
            { "user": { "username": "carol" }, "state": "unreviewed" },
            { "user": { "username": "dave" }, "state": "approved" }
        ])).unwrap();
        let approvals = serde_json::from_value(serde_json::json!({ "approved_by": [
            { "user": { "username": "dave" } }, { "user": { "username": "erin" } }
        ] })).unwrap();
        let reviewers = reviewers(&requested, &approvals);
        assert_eq!(
            reviewers
                .iter()
                .map(|reviewer| reviewer.verdict)
                .collect::<Vec<_>>(),
            [
                Verdict::ChangesRequested,
                Verdict::Commented,
                Verdict::Waiting,
                Verdict::Approved,
                Verdict::Approved
            ]
        );
    }
}
