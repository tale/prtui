use super::action::{Action, Motion};
use crate::expand::{self, Reveal};

/// The count typed ahead of a command, and what it means to the kinds of
/// command that read one.
///
/// This is everything a command is given. Nothing about the app reaches the
/// command table: a verb that has to look at state is a verb the app resolves,
/// not the keymap.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Count(Option<usize>);

impl Count {
    pub const fn new(count: Option<usize>) -> Self {
        Self(count)
    }

    /// A count on a motion is how many times to repeat it.
    const fn times(self) -> usize {
        match self.0 {
            Some(count) => count,
            None => 1,
        }
    }

    /// A count on a fold is how many lines to pull in, not how many times to
    /// repeat the command.
    fn lines(self) -> u32 {
        self.0.map_or(expand::STEP, |count| {
            u32::try_from(count).unwrap_or(u32::MAX)
        })
    }

    /// A count on a jump names the line to land on; without one the command
    /// means the end it is bound to.
    fn line_or(self, fallback: Motion) -> Action {
        Action::Move(self.0.map_or(fallback, Motion::Line))
    }
}

/// One named verb.
///
/// Keys and the command line both reach the app through this table, so a
/// binding is a name rather than another branch in the parser, and every
/// command is addressable as `:name` whether or not a key carries it.
pub struct Command {
    pub name: &'static str,
    pub build: fn(Count) -> Action,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "move-down",
        build: |count| Action::Move(Motion::Down(count.times())),
    },
    Command {
        name: "move-up",
        build: |count| Action::Move(Motion::Up(count.times())),
    },
    Command {
        name: "half-page-down",
        build: |_| Action::Move(Motion::HalfPageDown),
    },
    Command {
        name: "half-page-up",
        build: |_| Action::Move(Motion::HalfPageUp),
    },
    Command {
        name: "goto-first-line",
        build: |count| count.line_or(Motion::Top),
    },
    Command {
        name: "goto-last-line",
        build: |count| count.line_or(Motion::Bottom),
    },
    Command {
        name: "next-file",
        build: |count| Action::NextFile(count.times()),
    },
    Command {
        name: "prev-file",
        build: |count| Action::PrevFile(count.times()),
    },
    Command {
        name: "next-comment",
        build: |count| Action::NextComment(count.times()),
    },
    Command {
        name: "prev-comment",
        build: |count| Action::PrevComment(count.times()),
    },
    Command {
        name: "next-match",
        build: |count| Action::NextMatch(count.times()),
    },
    Command {
        name: "prev-match",
        build: |count| Action::PrevMatch(count.times()),
    },
    Command {
        name: "toggle-pane",
        build: |_| Action::TogglePane,
    },
    Command {
        name: "toggle-tree",
        build: |_| Action::ToggleTree,
    },
    Command {
        name: "focus-files",
        build: |_| Action::FocusFiles,
    },
    Command {
        name: "focus-diff",
        build: |_| Action::FocusDiff,
    },
    Command {
        name: "find",
        build: |_| Action::StartFind,
    },
    Command {
        name: "clear-find",
        build: |_| Action::ClearFind,
    },
    Command {
        name: "enter-visual",
        build: |_| Action::EnterVisual,
    },
    Command {
        name: "leave-visual",
        build: |_| Action::LeaveVisual,
    },
    Command {
        name: "comment",
        build: |_| Action::StartComment,
    },
    Command {
        name: "file-comment",
        build: |_| Action::StartFileComment,
    },
    Command {
        name: "edit-draft",
        build: |_| Action::EditDraft,
    },
    Command {
        name: "delete-draft",
        build: |_| Action::DeleteDraft,
    },
    Command {
        name: "toggle-resolved",
        build: |_| Action::ToggleResolved,
    },
    Command {
        name: "expand-up",
        build: |count| Action::Expand(Reveal::Up(count.lines())),
    },
    Command {
        name: "expand-down",
        build: |count| Action::Expand(Reveal::Down(count.lines())),
    },
    Command {
        name: "expand-all",
        build: |_| Action::Expand(Reveal::All),
    },
    Command {
        name: "expand-file",
        build: |_| Action::ExpandFile,
    },
    Command {
        name: "submit",
        build: |_| Action::StartSubmit,
    },
    Command {
        name: "next-verdict",
        build: |_| Action::CycleEvent(1),
    },
    Command {
        name: "prev-verdict",
        build: |_| Action::CycleEvent(-1),
    },
    Command {
        name: "command-line",
        build: |_| Action::StartCommandLine,
    },
    Command {
        name: "history-prev",
        build: |_| Action::WalkHistory(-1),
    },
    Command {
        name: "history-next",
        build: |_| Action::WalkHistory(1),
    },
    Command {
        name: "quit",
        build: |_| Action::Quit,
    },
    Command {
        name: "activate",
        build: |_| Action::Activate,
    },
    Command {
        name: "leave-card",
        build: |_| Action::LeaveThread,
    },
    Command {
        name: "commit-comment",
        build: |_| Action::CommitComment,
    },
    Command {
        name: "cancel-comment",
        build: |_| Action::CancelComment,
    },
    Command {
        name: "accept-filter",
        build: |_| Action::AcceptFileFilter,
    },
    Command {
        name: "cancel-filter",
        build: |_| Action::CancelFileFilter,
    },
    Command {
        name: "accept-search",
        build: |_| Action::AcceptSearch,
    },
    Command {
        name: "cancel-search",
        build: |_| Action::CancelSearch,
    },
    Command {
        name: "run-command-line",
        build: |_| Action::RunCommandLine,
    },
    Command {
        name: "cancel-command-line",
        build: |_| Action::CancelCommandLine,
    },
    Command {
        name: "commit-submit",
        build: |_| Action::CommitSubmit,
    },
    Command {
        name: "cancel-submit",
        build: |_| Action::CancelSubmit,
    },
    // Normal mode's escape is a precedence rule over state — leave the card,
    // else drop the query, else quit — so the app resolves it and the keymap
    // only names it.
    Command {
        name: "escape",
        build: |_| Action::Escape,
    },
];

pub fn find(name: &str) -> Option<&'static Command> {
    COMMANDS.iter().find(|command| command.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_command_is_addressable_by_exactly_one_name() {
        for command in COMMANDS {
            assert!(find(command.name).is_some());
        }

        let mut names: Vec<&str> =
            COMMANDS.iter().map(|command| command.name).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }
}
