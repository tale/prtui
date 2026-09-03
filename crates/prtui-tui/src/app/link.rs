//! Provider-neutral links requested by the application.

use std::sync::Arc;

/// A destination whose concrete URL is owned by the active provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    /// The pull request under review.
    PullRequest,
    /// A review conversation, addressed by an opaque provider identifier.
    Comment(Arc<str>),
    /// A file or line range at an immutable commit.
    Blob {
        commit: Arc<str>,
        path: Arc<str>,
        lines: Option<(u32, u32)>,
    },
}

/// Work the runtime performs outside the application state machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Errand {
    /// Resolve and open a provider link.
    Open(Link),
    /// Resolve and copy a provider link.
    Copy(Link),
}
