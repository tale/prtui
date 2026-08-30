use std::collections::HashSet;
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
    /// What the side is called to a reader rather than to the API. A diff has
    /// an old file and a new one; which way round `LEFT` and `RIGHT` go is
    /// GitHub's business, not the reviewer's.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "old",
            Self::Right => "new",
        }
    }
}

/// Where a comment lands in GitHub's diff coordinate system.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub start_line: u32,
    pub start_side: Side,
    pub end_line: u32,
    pub side: Side,
}

impl Anchor {
    pub(crate) const fn spanning(
        start_line: u32,
        end_line: u32,
        side: Side,
    ) -> Self {
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
    Review(Arc<str>),
    PullRequest(Arc<str>),
}

/// A draft ready to leave the application boundary.
///
/// `anchor` is absent for a file-level note. GitHub field names and enum
/// spellings are derived only by the wire layer.
#[derive(Debug, PartialEq, Eq)]
pub struct NewThread {
    pub parent: Parent,
    pub path: Arc<str>,
    pub body: String,
    pub anchor: Option<Anchor>,
}

/// The verdict a submitted review carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewEvent {
    #[default]
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewEvent {
    pub const ALL: [Self; 3] =
        [Self::Comment, Self::Approve, Self::RequestChanges];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Approve => "approve",
            Self::RequestChanges => "request changes",
        }
    }

    /// GitHub rejects a comment or change request with no summary.
    pub const fn requires_body(self) -> bool {
        matches!(self, Self::Comment | Self::RequestChanges)
    }

    #[must_use]
    pub fn step(self, direction: isize) -> Self {
        let count = Self::ALL.len();
        let position = Self::ALL
            .iter()
            .position(|event| *event == self)
            .unwrap_or(0);

        Self::ALL[(position + count).saturating_add_signed(direction) % count]
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
    /// The commit the head branch points at, which is the file contents a
    /// reader expands a diff against.
    pub head_oid: Arc<str>,
    pub body: String,
}

/// What one metadata fetch yields. The threads travel beside the pull request
/// rather than inside it: the app files them by path and nothing reads them in
/// the order they arrived.
#[derive(Debug, Clone)]
pub struct Meta {
    pub pr: PullRequest,
    pub threads: Vec<ReviewThread>,
    /// The pull request's own comments: the ones written about the change as a
    /// whole rather than against a line of it.
    pub discussion: Vec<Comment>,
    /// The review the viewer has open but not submitted, which every draft is
    /// filed against once the first one has opened it.
    pub pending_review: Option<Arc<str>>,
    /// Paths the viewer has already read through. A set rather than a flag on
    /// `ChangedFile`: the patches come from REST and the marks from GraphQL,
    /// and the two land independently.
    pub viewed: HashSet<Arc<str>>,
}

/// `@@ -old,count +new,count @@` — captures the two start line numbers.
pub(crate) fn parse_hunk_header(header: &str) -> Option<(u32, u32)> {
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
    pub review: Arc<str>,
    pub comment: Arc<str>,
}
