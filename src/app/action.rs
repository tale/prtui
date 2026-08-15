/// Cursor movements, expressed independently of which pane owns the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Down(usize),
    Up(usize),
    HalfPageDown,
    HalfPageUp,
    Top,
    Bottom,
}

/// The application's verb vocabulary.
///
/// App-level keys resolve to one of these, which keeps command parsing separate
/// from state transitions and testable without a terminal. Widget-internal
/// editing stays inside the input router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Move(Motion),
    NextFile,
    PrevFile,
    /// Jump to the next/previous unresolved thread, crossing into the
    /// following file with comments once the current one runs out.
    NextComment,
    PrevComment,
    TogglePane,
    ToggleTree,
    Activate,
    LeaveThread,
    FocusFiles,
    FocusDiff,
    /// `/` — filters the tree from the files pane, searches the open file from
    /// the diff pane. Which one is a property of app state, not of the key.
    StartFind,
    /// Drops whichever find state is live, leaving the view where it is.
    ClearFind,
    AcceptFileFilter,
    CancelFileFilter,
    AcceptSearch,
    CancelSearch,
    NextMatch,
    PrevMatch,

    EnterVisual,
    LeaveVisual,

    /// Open the composer for the cursor line, or the visual selection.
    StartComment,
    CommitComment,
    CancelComment,

    Quit,
}
