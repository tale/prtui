use crate::expand::Reveal;

/// Cursor movements, expressed independently of which pane owns the cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    Down(usize),
    Up(usize),
    HalfPageDown,
    HalfPageUp,
    Top,
    Bottom,
    /// A line by the number the gutter shows, which is the new side of the
    /// diff, or a row of the tree when the files pane has the cursor.
    Line(usize),
}

/// The application's verb vocabulary.
///
/// App-level keys resolve to one of these, which keeps command parsing separate
/// from state transitions and testable without a terminal. Widget-internal
/// editing stays inside the input router.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Move(Motion),
    /// Counts say how many stops to take. Every command that repeats carries
    /// one, so a count prefix means the same thing wherever it is typed.
    NextFile(usize),
    PrevFile(usize),
    /// Jump to the next/previous unresolved thread, crossing into the
    /// following file with comments once the current one runs out.
    NextComment(usize),
    PrevComment(usize),
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
    /// Back out of the innermost thing the reader is inside. Which one that is
    /// is a fact about app state, so the app decides rather than the keymap.
    Escape,
    AcceptFileFilter,
    CancelFileFilter,
    AcceptSearch,
    CancelSearch,
    NextMatch(usize),
    PrevMatch(usize),

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
    /// Pull part of the run of hidden lines the cursor rests on into the diff.
    Expand(Reveal),
    /// Pull in every run the open file's patch left out, at once.
    ExpandFile,

    /// Open the overlay that ships every draft as one review.
    StartSubmit,
    CommitSubmit,
    CancelSubmit,
    /// Step the verdict the review will carry.
    CycleEvent(isize),

    /// Open the `:` line, run what was typed into it, or drop it.
    StartCommandLine,
    RunCommandLine,
    CancelCommandLine,
    /// Walk the command history back (-1) or forward (1).
    WalkHistory(isize),

    /// Open the key reference, open the description and the discussion, or
    /// close whichever of the two is being read.
    OpenHelp,
    OpenOverview,
    CloseOverlay,

    /// Hand what the cursor is on to the browser, or put its link on the
    /// clipboard.
    OpenInBrowser,
    YankLink,

    Quit,
}
