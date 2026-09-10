//! File state specific to a local Git snapshot.

/// Where a file has changes within the local snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Staging {
    Staged,
    Unstaged,
    Mixed,
    Untracked,
}

impl Staging {
    /// Two columns representing staged and unstaged changes.
    pub const fn marker(self) -> &'static str {
        match self {
            Self::Staged => "S ",
            Self::Unstaged => " U",
            Self::Mixed => "SU",
            Self::Untracked => " ?",
        }
    }

    /// The full staging status of the selected file.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Unstaged => "unstaged",
            Self::Mixed => "staged + unstaged",
            Self::Untracked => "untracked",
        }
    }
}

/// Local file annotations kept separate from provider review state.
#[derive(Debug, Clone, Copy)]
pub struct LocalFile {
    pub staging: Staging,
    pub has_net_changes: bool,
}
