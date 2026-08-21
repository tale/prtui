use super::draft::Parent;
use super::editor::CommentEditor;
use std::sync::Arc;

/// The verdict a submitted review carries, matching GitHub's review events.
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

    pub const fn as_api(self) -> &'static str {
        match self {
            Self::Comment => "COMMENT",
            Self::Approve => "APPROVE",
            Self::RequestChanges => "REQUEST_CHANGES",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Comment => "comment",
            Self::Approve => "approve",
            Self::RequestChanges => "request changes",
        }
    }

    /// GitHub rejects a comment or a change request that carries no summary,
    /// however many inline comments ride along with it.
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

/// The submit overlay: the verdict plus the summary that accompanies it.
///
/// `error` is what GitHub said the last time this review went out. It lives
/// here rather than in the status bar because the bar has one line and a
/// validation failure names a field, a rule and an offending value.
#[derive(Default)]
pub struct Submission {
    pub editor: CommentEditor,
    pub event: ReviewEvent,
    pub error: Option<String>,
    /// Set by an escape that had a summary to lose. The next escape discards;
    /// any other key clears it.
    pub is_discard_armed: bool,
}

/// Work that has to leave the process. The app queues these rather than
/// reaching for the network itself, which keeps every state transition
/// synchronous and testable.
///
/// Every draft request names the draft by its local id, since the answer has to
/// find its way back to a draft that was already on screen before it left.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    AddThread {
        draft: u64,
        parent: Parent,
        input: serde_json::Value,
    },
    UpdateComment {
        draft: u64,
        comment: Arc<str>,
        body: String,
    },
    DeleteComment {
        draft: u64,
        comment: Arc<str>,
    },
    Review {
        parent: Parent,
        event: ReviewEvent,
        body: String,
    },
    Reply {
        in_reply_to: u64,
        body: String,
    },
    Resolve {
        thread_id: Arc<str>,
        is_resolved: bool,
    },
}

/// What a completed request retires, so the app knows which local state the
/// server has now taken over.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sent {
    ThreadAdded {
        draft: u64,
        review: Arc<str>,
        comment: Arc<str>,
    },
    CommentUpdated(u64),
    CommentDeleted(u64),
    Review,
    Reply,
    Resolution(bool),
}

/// Why a request came back empty-handed.
///
/// A review leaves its summary behind, and a draft is left marked as ahead of
/// the server; both have to be handed back rather than dropped, so both are
/// told apart from the failures that only need reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Failure {
    Draft(u64, String),
    Review(String),
    Other(String),
}

impl Failure {
    pub fn message(&self) -> &str {
        let (Self::Draft(_, message)
        | Self::Review(message)
        | Self::Other(message)) = self;

        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_the_verdict_wraps_both_ways() {
        assert_eq!(ReviewEvent::Comment.step(-1), ReviewEvent::RequestChanges);
        assert_eq!(ReviewEvent::RequestChanges.step(1), ReviewEvent::Comment);
        assert_eq!(ReviewEvent::Comment.step(1), ReviewEvent::Approve);
        assert_eq!(ReviewEvent::Approve.step(-1), ReviewEvent::Comment);
    }
}
