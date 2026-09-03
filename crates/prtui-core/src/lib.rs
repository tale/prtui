//! Provider-neutral data exchanged by prtui providers and views.
//!
//! Provider implementations translate their API's wire types into these
//! models and implement [`Provider`]. The TUI never reads provider-specific
//! response objects.

#![deny(missing_docs)]

mod provider;

pub use provider::Provider;

use std::collections::HashSet;
use std::sync::Arc;

/// How a line participates in a unified diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// An unchanged line shown for context.
    Context,
    /// A line present only in the new file.
    Added,
    /// A line present only in the old file.
    Removed,
    /// A hunk header describing the following line ranges.
    Hunk,
}

/// Which side of a diff a review thread is anchored to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    /// The old version of the file.
    Left,
    /// The new version of the file.
    Right,
}

impl Side {
    /// Returns the reader-facing name of this side.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "old",
            Self::Right => "new",
        }
    }
}

/// The inclusive diff range to which a comment is attached.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// First line in the range.
    pub start_line: u32,
    /// Diff side containing the first line.
    pub start_side: Side,
    /// Last line in the range.
    pub end_line: u32,
    /// Diff side containing the last line.
    pub side: Side,
}

impl Anchor {
    /// Creates an anchor spanning two lines on the same diff side.
    pub const fn spanning(start_line: u32, end_line: u32, side: Side) -> Self {
        Self {
            start_line,
            start_side: side,
            end_line,
            side,
        }
    }

    /// A span crossing sides remains multi-line even when both sides use the
    /// same line number.
    pub fn is_multiline(self) -> bool {
        self.start_line != self.end_line || self.start_side != self.side
    }
}

/// What a new draft hangs off. The first draft opens a review against the pull
/// request; later drafts join the review that first response named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Parent {
    /// An existing pending review, identified by the provider.
    Review(Arc<str>),
    /// A pull request, identified by the provider.
    PullRequest(Arc<str>),
}

/// A draft ready to leave the application boundary.
///
/// `anchor` is absent for a file-level note. Providers derive their outbound
/// wire representation from this value.
#[derive(Debug, PartialEq, Eq)]
pub struct NewThread {
    /// Review or pull request that owns the new thread.
    pub parent: Parent,
    /// Repository-relative file path.
    pub path: Arc<str>,
    /// Markdown comment body.
    pub body: String,
    /// Diff range, or `None` for a file-level comment.
    pub anchor: Option<Anchor>,
}

/// The verdict a submitted review carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewEvent {
    #[default]
    /// Submit feedback without a verdict.
    Comment,
    /// Approve the pull request.
    Approve,
    /// Request changes before merge.
    RequestChanges,
}

impl ReviewEvent {
    /// Events in display order.
    pub const ALL: [Self; 3] =
        [Self::Comment, Self::Approve, Self::RequestChanges];

    /// Returns the reader-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Approve => "approve",
            Self::RequestChanges => "request changes",
        }
    }

    /// Whether the application requires a summary for this event.
    pub const fn requires_body(self) -> bool {
        matches!(self, Self::Comment | Self::RequestChanges)
    }

    #[must_use]
    /// Moves through [`Self::ALL`], wrapping in either direction.
    pub fn step(self, direction: isize) -> Self {
        let count = Self::ALL.len();
        let position = Self::ALL
            .iter()
            .position(|event| *event == self)
            .unwrap_or(0);

        Self::ALL[(position + count).saturating_add_signed(direction) % count]
    }
}

/// One parsed line of a unified diff.
#[derive(Debug, Clone)]
pub struct DiffLine {
    /// Role of this line in the diff.
    pub kind: LineKind,
    /// Line content without a diff marker.
    pub text: String,
    /// Line number in the old file, when present.
    pub old_line: Option<u32>,
    /// Line number in the new file, when present.
    pub new_line: Option<u32>,
}

/// A changed file and the diff rows the view renders.
#[derive(Debug, Clone)]
pub struct ChangedFile {
    /// Shared: a file's path is its identity, and the threads, drafts and
    /// syntax colors filed against it all hold the same one.
    pub path: Arc<str>,
    /// Provider-normalized change status, such as `modified` or `removed`.
    pub status: String,
    /// Number of added lines.
    pub additions: u32,
    /// Number of removed lines.
    pub deletions: u32,
    /// Parsed unified-diff rows.
    pub lines: Vec<DiffLine>,
}

impl ChangedFile {
    /// Whether the provider returned line counts but withheld the patch.
    pub const fn is_patch_withheld(&self) -> bool {
        self.lines.is_empty() && (self.additions > 0 || self.deletions > 0)
    }
}

/// A provider-normalized review or discussion comment.
#[derive(Debug, Clone)]
pub struct Comment {
    /// Opaque provider identifier used to update or delete the comment.
    pub id: Arc<str>,
    /// Opaque provider identifier used to reply to or link to the comment.
    pub reply_target: Option<Arc<str>>,
    /// Display name of the comment author.
    pub author: String,
    /// Markdown comment body.
    pub body: String,
    /// Provider-formatted creation time.
    pub created_at: String,
    /// A comment nobody but its author can see yet, because the review holding
    /// it has not been submitted.
    pub is_pending: bool,
}

#[derive(Debug, Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "each flag describes an independent thread capability or state"
)]
/// A conversation attached to a file or diff range.
pub struct ReviewThread {
    /// Opaque provider identifier for the thread.
    pub id: Arc<str>,
    /// Repository-relative file path.
    pub path: Arc<str>,
    /// Current ending line, when the anchor still exists.
    pub line: Option<u32>,
    /// Original ending line, used when the anchor is outdated.
    pub original_line: Option<u32>,
    /// First line of a multi-line range.
    pub start_line: Option<u32>,
    /// Diff side containing the ending line.
    pub side: Side,
    /// Null for a thread that covers one line, where the start side is the
    /// only side there is.
    pub start_side: Option<Side>,
    /// A remark on the file rather than on any line in it.
    pub is_file_level: bool,
    /// Whether the conversation has been resolved.
    pub is_resolved: bool,
    /// Whether the attached diff range is no longer current.
    pub is_outdated: bool,
    /// Whether the current viewer may change the resolution.
    pub can_resolve: bool,
    /// Comments in conversation order.
    pub comments: Vec<Comment>,
}

impl ReviewThread {
    /// A thread is pending exactly when its first comment is, since a reply
    /// cannot be filed against a review that has not been submitted.
    pub fn is_pending(&self) -> bool {
        self.comments.first().is_some_and(|first| first.is_pending)
    }

    /// Returns the current line, falling back to the original outdated line.
    pub fn anchor_line(&self) -> Option<u32> {
        self.line.or(self.original_line)
    }

    /// Returns the provider's reply target for the conversation root.
    pub fn reply_target(&self) -> Option<Arc<str>> {
        self.comments
            .first()
            .and_then(|comment| comment.reply_target.clone())
    }

    /// Whether the line is the thread's display anchor.
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

/// Pull request metadata rendered by the review surface.
#[derive(Debug, Clone, Default)]
pub struct PullRequest {
    /// Opaque provider identifier used for pull-request mutations.
    pub id: Arc<str>,
    /// Repository-local pull request number.
    pub number: u32,
    /// Pull request title.
    pub title: String,
    /// Provider-normalized state label.
    pub state: String,
    /// Whether the pull request is a draft.
    pub is_draft: bool,
    /// Display name of the author.
    pub author: String,
    /// Base branch name.
    pub base_ref: String,
    /// Head branch name.
    pub head_ref: String,
    /// The commit the head branch points at, which is the file contents a
    /// reader expands a diff against.
    pub head_oid: Arc<str>,
    /// Markdown pull request description.
    pub body: String,
}

/// What one metadata fetch yields. The threads travel beside the pull request
/// rather than inside it: the app files them by path and nothing reads them in
/// the order they arrived.
#[derive(Debug, Clone)]
pub struct Meta {
    /// Pull request metadata.
    pub pr: PullRequest,
    /// Review conversations across all changed files.
    pub threads: Vec<ReviewThread>,
    /// The pull request's own comments: the ones written about the change as a
    /// whole rather than against a line of it.
    pub discussion: Vec<Comment>,
    /// The review the viewer has open but not submitted, which every draft is
    /// filed against once the first one has opened it.
    pub pending_review: Option<Arc<str>>,
    /// Paths the viewer has already read through.
    pub viewed: HashSet<Arc<str>>,
}

/// `@@ -old,count +new,count @@` — captures the two start line numbers.
pub fn parse_hunk_header(header: &str) -> Option<(u32, u32)> {
    let inner = header.strip_prefix("@@ ")?.split(" @@").next()?;
    let (old, new) = inner.split_once(' ')?;

    let start = |s: &str| -> Option<u32> {
        s.get(1..)?.split(',').next()?.parse().ok()
    };

    Some((start(old)?, start(new)?))
}

/// What filing a draft comment hands back: the comment to address later edits
/// to, and the review it was filed against, which the next draft reuses.
#[derive(Debug)]
pub struct AddedThread {
    /// Opaque identifier of the pending review.
    pub review: Arc<str>,
    /// Opaque identifier of the created comment.
    pub comment: Arc<str>,
}

/// Repository identity shared by every provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repo {
    /// Explicit provider host, or `None` for that provider's default host.
    pub host: Option<String>,
    /// Repository namespace or owner.
    pub namespace: String,
    /// Repository name.
    pub name: String,
}

impl Repo {
    /// Returns `[HOST/]NAMESPACE/NAME`.
    pub fn slug(&self) -> String {
        match &self.host {
            Some(host) => format!("{host}/{}/{}", self.namespace, self.name),
            None => format!("{}/{}", self.namespace, self.name),
        }
    }
}

/// A pull request located within a repository.
#[derive(Clone)]
pub struct PullRequestTarget {
    /// Repository containing the pull request.
    pub repo: Arc<Repo>,
    /// Repository-local pull request number.
    pub number: u32,
}

/// One row in a provider-produced pull request listing.
pub struct PullRequestListItem {
    /// Repository and number opened by this row.
    pub target: PullRequestTarget,
    /// Pull request title.
    pub title: String,
    /// Display name of the author.
    pub author: String,
    /// Review state shown in the listing.
    pub review_status: ReviewStatus,
}

/// Scope represented by a pull request listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PullRequestListScope {
    /// Open pull requests from one repository.
    Repository,
    /// Open pull requests relevant to the current user.
    User,
}

/// A complete pull request listing ready for the selector view.
pub struct PullRequestList {
    /// Scope used to label and lay out the listing.
    pub scope: PullRequestListScope,
    /// Rows in provider-defined display order.
    pub items: Vec<PullRequestListItem>,
}

impl PullRequestList {
    /// Returns the number of rows.
    pub const fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the listing contains no rows.
    pub const fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns a row by display index.
    pub fn get(&self, index: usize) -> Option<&PullRequestListItem> {
        self.items.get(index)
    }

    /// Consumes the listing and returns the selected target.
    pub fn select(mut self, index: usize) -> Option<PullRequestTarget> {
        if index >= self.items.len() {
            return None;
        }

        Some(self.items.swap_remove(index).target)
    }
}

/// Provider-normalized review state for a pull request row.
pub enum ReviewStatus {
    /// Pull request is still a draft.
    Draft,
    /// A reviewer requested changes.
    ChangesRequested,
    /// The pull request is waiting for review.
    ReviewRequired,
    /// The latest decision approved the change.
    Approved,
    /// No review decision is available.
    NoDecision,
}

/// Pull request facts rendered by the selector's summary panel.
pub struct Summary {
    /// Display name of the author.
    pub author: String,
    /// Base branch name.
    pub base_ref: String,
    /// Head branch name.
    pub head_ref: String,
    /// Number of added lines.
    pub additions: u32,
    /// Number of removed lines.
    pub deletions: u32,
    /// Number of changed files.
    pub changed_files: u32,
    /// Provider-formatted last update date.
    pub updated_on: String,
    /// Number of pull request discussion comments.
    pub comments: u32,
    /// Checks in provider-defined display order.
    pub checks: Vec<Check>,
    /// Requested and completed reviewers.
    pub reviewers: Vec<Reviewer>,
    /// Review conversation counts.
    pub threads: Threads,
}

/// One continuous-integration check.
pub struct Check {
    /// Display name of the check.
    pub name: String,
    /// Normalized check state.
    pub state: CheckState,
}

/// Provider-normalized state of a check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    /// Check completed unsuccessfully.
    Failed,
    /// Check has not completed.
    Running,
    /// Check completed successfully.
    Passed,
    /// Check did not run or has no actionable result.
    Skipped,
}

/// A user or team participating in review.
pub struct Reviewer {
    /// Display name of the user or team.
    pub name: String,
    /// Whether the reviewer represents a team.
    pub is_team: bool,
    /// Latest normalized review verdict.
    pub verdict: Verdict,
}

/// Provider-normalized verdict from a reviewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    /// Reviewer requested changes.
    ChangesRequested,
    /// Review has been requested but not completed.
    Waiting,
    /// Reviewer commented without approving or requesting changes.
    Commented,
    /// Reviewer approved the pull request.
    Approved,
}

impl Verdict {
    /// Returns the reader-facing label.
    pub const fn label(self) -> &'static str {
        match self {
            Self::ChangesRequested => "changes requested",
            Self::Waiting => "waiting",
            Self::Commented => "commented",
            Self::Approved => "approved",
        }
    }
}

/// Aggregate review conversation counts.
pub struct Threads {
    /// Number of unresolved conversations.
    pub unresolved: u32,
    /// Total number of conversations represented.
    pub total: u32,
    /// Whether `total` is a lower bound because the provider truncated data.
    pub is_truncated: bool,
}
