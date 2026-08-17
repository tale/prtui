use super::editor::CommentEditor;

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
#[derive(Default)]
pub struct Submission {
    pub editor: CommentEditor,
    pub event: ReviewEvent,
}

/// Work that has to leave the process. The app queues these rather than
/// reaching for the network itself, which keeps every state transition
/// synchronous and testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Review {
        event: ReviewEvent,
        body: String,
        comments: Vec<serde_json::Value>,
    },
    Reply {
        in_reply_to: u64,
        body: String,
    },
    Resolve {
        thread_id: String,
        is_resolved: bool,
    },
}

/// What a completed request retires, so the app knows which local state the
/// server has now taken over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sent {
    /// The number of drafts the review carried, which are the oldest ones in
    /// the list; anything written since is still pending.
    Review(usize),
    Reply,
    Resolution(bool),
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
