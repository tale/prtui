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

/// The application's verb vocabulary. App-level keys resolve to one of these,
/// which keeps command parsing separate from state transitions and testable
/// without a terminal. Widget-internal editing stays inside the input router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Move(Motion),
    NextFile,
    PrevFile,
    TogglePane,
    ToggleTree,

    EnterVisual,
    LeaveVisual,

    /// Open the composer for the cursor line, or the visual selection.
    StartComment,
    CommitComment,
    CancelComment,

    Quit,
}
