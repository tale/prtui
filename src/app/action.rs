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

    /// Open the composer for the cursor line, the visual selection, or the
    /// focused thread, whichever the cursor is on.
    StartComment,
    /// Open the composer for the whole file, for a remark that belongs to no
    /// particular line. Reopens the file's existing draft if it has one.
    StartFileComment,
    CommitComment,
    CancelComment,
    /// Reopen the draft covering the cursor line.
    EditDraft,
    /// Throw away the draft covering the cursor line.
    DeleteDraft,
    /// Resolve the focused thread, or reopen it if it is already resolved.
    ToggleResolved,

    /// Open the overlay that ships every draft as one review.
    StartSubmit,
    CommitSubmit,
    CancelSubmit,
    /// Step the verdict the review will carry.
    CycleEvent(isize),

    Quit,
}
