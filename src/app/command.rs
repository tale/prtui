use super::action::{Action, Edit, Motion};
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
///
/// `group` and `summary` are what the key reference is drawn from, which is why
/// the table is ordered for reading rather than alphabetically: commands of one
/// group sit together, and the groups run from the keys used most to the keys
/// used least.
pub struct Command {
    pub name: &'static str,
    pub group: &'static str,
    pub summary: &'static str,
    pub build: fn(Count) -> Action,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "move-down",
        group: "motion",
        summary: "down one row",
        build: |count| Action::Move(Motion::Down(count.times())),
    },
    Command {
        name: "move-up",
        group: "motion",
        summary: "up one row",
        build: |count| Action::Move(Motion::Up(count.times())),
    },
    Command {
        name: "half-page-down",
        group: "motion",
        summary: "down half a screen",
        build: |_| Action::Move(Motion::HalfPageDown),
    },
    Command {
        name: "half-page-up",
        group: "motion",
        summary: "up half a screen",
        build: |_| Action::Move(Motion::HalfPageUp),
    },
    Command {
        name: "goto-first-line",
        group: "motion",
        summary: "the first line, or line {count}",
        build: |count| count.line_or(Motion::Top),
    },
    Command {
        name: "goto-last-line",
        group: "motion",
        summary: "the last line, or line {count}",
        build: |count| count.line_or(Motion::Bottom),
    },
    Command {
        name: "next-file",
        group: "files",
        summary: "open the next file",
        build: |count| Action::NextFile(count.times()),
    },
    Command {
        name: "prev-file",
        group: "files",
        summary: "open the previous file",
        build: |count| Action::PrevFile(count.times()),
    },
    Command {
        name: "toggle-tree",
        group: "files",
        summary: "show or hide the file tree",
        build: |_| Action::ToggleTree,
    },
    Command {
        name: "toggle-pane",
        group: "files",
        summary: "swap the focused pane",
        build: |_| Action::TogglePane,
    },
    Command {
        name: "focus-files",
        group: "files",
        summary: "focus the file tree",
        build: |_| Action::FocusFiles,
    },
    Command {
        name: "focus-diff",
        group: "files",
        summary: "focus the diff",
        build: |_| Action::FocusDiff,
    },
    Command {
        name: "toggle-viewed",
        group: "files",
        summary: "mark read and open the next unread",
        build: |_| Action::ToggleViewed,
    },
    Command {
        name: "activate",
        group: "files",
        summary: "open or expand what the cursor is on",
        build: |_| Action::Activate,
    },
    Command {
        name: "next-comment",
        group: "conversations",
        summary: "the next unanswered conversation",
        build: |count| Action::NextComment(count.times()),
    },
    Command {
        name: "prev-comment",
        group: "conversations",
        summary: "the previous one",
        build: |count| Action::PrevComment(count.times()),
    },
    Command {
        name: "leave-card",
        group: "conversations",
        summary: "give the focus back to the code",
        build: |_| Action::LeaveThread,
    },
    Command {
        name: "toggle-resolved",
        group: "conversations",
        summary: "resolve or reopen the conversation",
        build: |_| Action::ToggleResolved,
    },
    Command {
        name: "find",
        group: "find",
        summary: "search the panel, tree, or file",
        build: |_| Action::StartFind,
    },
    Command {
        name: "next-match",
        group: "find",
        summary: "the next hit",
        build: |count| Action::NextMatch(count.times()),
    },
    Command {
        name: "prev-match",
        group: "find",
        summary: "the previous hit",
        build: |count| Action::PrevMatch(count.times()),
    },
    Command {
        name: "clear-find",
        group: "find",
        summary: "drop the filter or the search",
        build: |_| Action::ClearFind,
    },
    Command {
        name: "accept-filter",
        group: "find",
        summary: "keep the filter and leave it",
        build: |_| Action::AcceptFileFilter,
    },
    Command {
        name: "cancel-filter",
        group: "find",
        summary: "put back what the filter replaced",
        build: |_| Action::CancelFileFilter,
    },
    Command {
        name: "accept-search",
        group: "find",
        summary: "keep the search and leave it",
        build: |_| Action::AcceptSearch,
    },
    Command {
        name: "cancel-search",
        group: "find",
        summary: "go back to where the search began",
        build: |_| Action::CancelSearch,
    },
    Command {
        name: "expand-down",
        group: "hidden lines",
        summary: "reveal hidden lines downward",
        build: |count| Action::Expand(Reveal::Down(count.lines())),
    },
    Command {
        name: "expand-up",
        group: "hidden lines",
        summary: "reveal hidden lines upward",
        build: |count| Action::Expand(Reveal::Up(count.lines())),
    },
    Command {
        name: "expand-all",
        group: "hidden lines",
        summary: "reveal the run under the cursor",
        build: |_| Action::Expand(Reveal::All),
    },
    Command {
        name: "expand-file",
        group: "hidden lines",
        summary: "reveal every hidden line in the file",
        build: |_| Action::ExpandFile,
    },
    Command {
        name: "comment",
        group: "comments",
        summary: "comment on the line, span, or thread",
        build: |_| Action::StartComment,
    },
    Command {
        name: "file-comment",
        group: "comments",
        summary: "write a note about the whole file",
        build: |_| Action::StartFileComment,
    },
    Command {
        name: "edit-draft",
        group: "comments",
        summary: "reopen the draft under the cursor",
        build: |_| Action::EditDraft,
    },
    Command {
        name: "delete-draft",
        group: "comments",
        summary: "discard the draft under the cursor",
        build: |_| Action::DeleteDraft,
    },
    Command {
        name: "enter-visual",
        group: "comments",
        summary: "select the lines to comment on",
        build: |_| Action::EnterVisual,
    },
    Command {
        name: "leave-visual",
        group: "comments",
        summary: "drop the selection",
        build: |_| Action::LeaveVisual,
    },
    Command {
        name: "commit-comment",
        group: "comments",
        summary: "save the comment",
        build: |_| Action::CommitComment,
    },
    Command {
        name: "cancel-comment",
        group: "comments",
        summary: "close the composer",
        build: |_| Action::CancelComment,
    },
    Command {
        name: "submit",
        group: "review",
        summary: "open the form that ships every draft",
        build: |_| Action::StartSubmit,
    },
    Command {
        name: "next-verdict",
        group: "review",
        summary: "step the verdict forward",
        build: |_| Action::CycleEvent(1),
    },
    Command {
        name: "prev-verdict",
        group: "review",
        summary: "step the verdict back",
        build: |_| Action::CycleEvent(-1),
    },
    Command {
        name: "commit-submit",
        group: "review",
        summary: "send the review",
        build: |_| Action::CommitSubmit,
    },
    Command {
        name: "cancel-submit",
        group: "review",
        summary: "close the form",
        build: |_| Action::CancelSubmit,
    },
    Command {
        name: "command-line",
        group: "command line",
        summary: "type a command, or a line to jump to",
        build: |_| Action::StartCommandLine,
    },
    Command {
        name: "run-command-line",
        group: "command line",
        summary: "run what was typed",
        build: |_| Action::RunCommandLine,
    },
    Command {
        name: "cancel-command-line",
        group: "command line",
        summary: "close the command line",
        build: |_| Action::CancelCommandLine,
    },
    Command {
        name: "history-prev",
        group: "command line",
        summary: "an older command",
        build: |_| Action::WalkHistory(-1),
    },
    Command {
        name: "history-next",
        group: "command line",
        summary: "a newer command",
        build: |_| Action::WalkHistory(1),
    },
    Command {
        name: "line-start",
        group: "prompt",
        summary: "the start of the line",
        build: |_| Action::EditLine(Edit::LineStart),
    },
    Command {
        name: "line-end",
        group: "prompt",
        summary: "the end of it",
        build: |_| Action::EditLine(Edit::LineEnd),
    },
    Command {
        name: "char-left",
        group: "prompt",
        summary: "back one character",
        build: |_| Action::EditLine(Edit::CharLeft),
    },
    Command {
        name: "char-right",
        group: "prompt",
        summary: "on one character",
        build: |_| Action::EditLine(Edit::CharRight),
    },
    Command {
        name: "word-left",
        group: "prompt",
        summary: "back one word",
        build: |_| Action::EditLine(Edit::WordLeft),
    },
    Command {
        name: "word-right",
        group: "prompt",
        summary: "on one word",
        build: |_| Action::EditLine(Edit::WordRight),
    },
    Command {
        name: "delete-char",
        group: "prompt",
        summary: "rub out the character ahead",
        build: |_| Action::EditLine(Edit::DeleteChar),
    },
    Command {
        name: "delete-word-left",
        group: "prompt",
        summary: "rub out the word behind",
        build: |_| Action::EditLine(Edit::DeleteWordLeft),
    },
    Command {
        name: "delete-word-right",
        group: "prompt",
        summary: "rub out the word ahead",
        build: |_| Action::EditLine(Edit::DeleteWordRight),
    },
    Command {
        name: "delete-to-blank",
        group: "prompt",
        summary: "rub out back to the last blank",
        build: |_| Action::EditLine(Edit::DeleteToBlank),
    },
    Command {
        name: "delete-to-start",
        group: "prompt",
        summary: "rub out to the start of the line",
        build: |_| Action::EditLine(Edit::DeleteToStart),
    },
    Command {
        name: "delete-to-end",
        group: "prompt",
        summary: "rub out to the end of it",
        build: |_| Action::EditLine(Edit::DeleteToEnd),
    },
    Command {
        name: "open",
        group: "links",
        summary: "open the pull request in a browser",
        build: |_| Action::OpenInBrowser,
    },
    Command {
        name: "yank",
        group: "links",
        summary: "copy a permalink to it",
        build: |_| Action::YankLink,
    },
    Command {
        name: "overview",
        group: "prtui",
        summary: "the description and the discussion",
        build: |_| Action::OpenOverview,
    },
    Command {
        name: "help",
        group: "prtui",
        summary: "this list",
        build: |_| Action::OpenHelp,
    },
    Command {
        name: "close-panel",
        group: "prtui",
        summary: "close whichever panel is open",
        build: |_| Action::CloseOverlay,
    },
    // Normal mode's escape is a precedence rule over state — leave the card,
    // else drop the query — so the app resolves it and the keymap only names
    // it.
    Command {
        name: "escape",
        group: "prtui",
        summary: "back out of the innermost thing",
        build: |_| Action::Escape,
    },
    Command {
        name: "quit",
        group: "prtui",
        summary: "leave prtui",
        build: |_| Action::Quit,
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

    /// The reference emits a heading whenever the group changes, so a group
    /// split in two would be headed twice.
    #[test]
    fn each_group_is_written_once() {
        let mut seen: Vec<&str> = Vec::new();

        for command in COMMANDS {
            if seen.last() == Some(&command.group) {
                continue;
            }
            assert!(
                !seen.contains(&command.group),
                "{} is split in two",
                command.group
            );
            seen.push(command.group);
        }
    }
}
