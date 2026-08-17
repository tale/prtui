use super::action::{Action, Motion};
use super::mode::Mode;
use termina::event::{KeyCode, KeyEvent, Modifiers};

const MAX_COUNT: usize = 999_999;

/// The result of feeding one key into the command parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    Action(Action),
    /// The key is a valid prefix, but the command needs more input.
    Pending,
    /// The key does not belong to the application keymap in this mode.
    Unbound,
}

/// Holds the half-typed parts of a command: a count prefix (`12j`) and a
/// pending operator (`g` awaiting its second `g`).
#[derive(Default)]
pub struct Keymap {
    count: Option<usize>,
    operator: Option<char>,
}

impl Keymap {
    pub fn pending_hint(&self) -> String {
        let count = self.count.map(|c| c.to_string()).unwrap_or_default();
        let operator = self.operator.map(String::from).unwrap_or_default();

        format!("{count}{operator}")
    }

    fn take_count(&mut self) -> usize {
        self.count.take().unwrap_or(1)
    }

    pub(super) const fn clear(&mut self) {
        self.count = None;
        self.operator = None;
    }

    /// `find_active` is true while either find surface — the tree filter or the
    /// diff search — holds a query, which is what escape clears.
    pub fn resolve(
        &mut self,
        mode: Mode,
        find_active: bool,
        key: KeyEvent,
    ) -> Resolution {
        if mode == Mode::Insert {
            return Self::resolve_insert(key);
        }
        if mode == Mode::Filter {
            return Self::resolve_filter(key);
        }
        if mode == Mode::Search {
            return Self::resolve_search(key);
        }
        if mode == Mode::Submit {
            return Self::resolve_submit(key);
        }

        let KeyCode::Char(c) = key.code else {
            return self.resolve_special(mode, find_active, key);
        };

        if key.modifiers == Modifiers::CONTROL {
            return self.resolve_control(mode, find_active, c);
        }

        // Shift is represented both by the character's case and, depending on
        // the terminal, by this flag. Other modifiers must not trigger a plain
        // application binding.
        if key.modifiers != Modifiers::NONE && key.modifiers != Modifiers::SHIFT
        {
            self.clear();
            return Resolution::Unbound;
        }

        // A leading zero is not a count prefix in a Vim-style keymap.
        if c.is_ascii_digit() && !(c == '0' && self.count.is_none()) {
            let digit = c.to_digit(10).unwrap() as usize;
            self.count = Some(
                self.count
                    .unwrap_or(0)
                    .saturating_mul(10)
                    .saturating_add(digit)
                    .min(MAX_COUNT),
            );
            return Resolution::Pending;
        }

        if self.operator == Some('g') {
            self.clear();
            return match c {
                'g' => Resolution::Action(Action::Move(Motion::Top)),
                _ => Resolution::Unbound,
            };
        }

        let count = self.take_count();
        let action = match c {
            'g' => {
                self.operator = Some('g');
                return Resolution::Pending;
            }
            'j' => Action::Move(Motion::Down(count)),
            'k' => Action::Move(Motion::Up(count)),
            'G' => Action::Move(Motion::Bottom),
            ']' => Action::NextFile,
            '[' => Action::PrevFile,
            'n' if mode == Mode::Normal => Action::NextMatch,
            'N' if mode == Mode::Normal => Action::PrevMatch,
            '}' if mode == Mode::Normal => Action::NextComment,
            '{' if mode == Mode::Normal => Action::PrevComment,
            'f' => Action::ToggleTree,
            '/' if mode == Mode::Normal => Action::StartFind,
            'h' if mode == Mode::Normal => Action::FocusFiles,
            'l' if mode == Mode::Normal => Action::FocusDiff,
            'v' | 'V' => {
                if mode == Mode::Visual {
                    Action::LeaveVisual
                } else {
                    Action::EnterVisual
                }
            }
            'c' => Action::StartComment,
            'e' if mode == Mode::Normal => Action::EditDraft,
            'd' if mode == Mode::Normal => Action::DeleteDraft,
            'R' if mode == Mode::Normal => Action::ToggleResolved,
            's' if mode == Mode::Normal => Action::StartSubmit,
            'q' => Action::Quit,
            _ => {
                self.clear();
                return Resolution::Unbound;
            }
        };

        self.clear();
        Resolution::Action(action)
    }

    const fn resolve_control(
        &mut self,
        mode: Mode,
        find_active: bool,
        c: char,
    ) -> Resolution {
        self.clear();

        match c {
            'c' => Resolution::Action(Action::Quit),
            'd' => Resolution::Action(Action::Move(Motion::HalfPageDown)),
            'u' => Resolution::Action(Action::Move(Motion::HalfPageUp)),
            '[' => Self::resolve_escape(mode, find_active),
            _ => Resolution::Unbound,
        }
    }

    fn resolve_special(
        &mut self,
        mode: Mode,
        find_active: bool,
        key: KeyEvent,
    ) -> Resolution {
        self.clear();

        if key.modifiers != Modifiers::NONE {
            return Resolution::Unbound;
        }

        match key.code {
            KeyCode::Tab => Resolution::Action(Action::TogglePane),
            KeyCode::Escape => Self::resolve_escape(mode, find_active),
            KeyCode::Enter if mode == Mode::Normal => {
                Resolution::Action(Action::Activate)
            }
            KeyCode::Right if mode == Mode::Normal => {
                Resolution::Action(Action::FocusDiff)
            }
            KeyCode::Left if mode == Mode::Normal => {
                Resolution::Action(Action::FocusFiles)
            }
            KeyCode::Down => Resolution::Action(Action::Move(Motion::Down(1))),
            KeyCode::Up => Resolution::Action(Action::Move(Motion::Up(1))),
            _ => Resolution::Unbound,
        }
    }

    const fn resolve_escape(mode: Mode, find_active: bool) -> Resolution {
        Resolution::Action(match mode {
            Mode::Normal if find_active => Action::ClearFind,
            Mode::Normal => Action::Quit,
            Mode::Visual => Action::LeaveVisual,
            Mode::Insert => Action::CancelComment,
            Mode::Filter => Action::CancelFileFilter,
            Mode::Search => Action::CancelSearch,
            Mode::Submit => Action::CancelSubmit,
        })
    }

    /// The verdict has to be reachable without stealing letters from the
    /// summary, so it moves on tab rather than on a mnemonic.
    fn resolve_submit(key: KeyEvent) -> Resolution {
        if key.modifiers == Modifiers::CONTROL {
            return match key.code {
                KeyCode::Char('c') => Resolution::Action(Action::Quit),
                KeyCode::Char('[') => Self::resolve_escape(Mode::Submit, false),
                _ => Resolution::Unbound,
            };
        }

        match (key.code, key.modifiers) {
            (KeyCode::Escape, Modifiers::NONE) => {
                Self::resolve_escape(Mode::Submit, false)
            }
            (KeyCode::Enter, Modifiers::NONE) => {
                Resolution::Action(Action::CommitSubmit)
            }
            (KeyCode::Tab, Modifiers::NONE) => {
                Resolution::Action(Action::CycleEvent(1))
            }
            (KeyCode::BackTab, _) | (KeyCode::Tab, Modifiers::SHIFT) => {
                Resolution::Action(Action::CycleEvent(-1))
            }
            _ => Resolution::Unbound,
        }
    }

    /// Searching mirrors filtering: printable keys build the query while the
    /// match-stepping and lifecycle keys stay application-level.
    fn resolve_search(key: KeyEvent) -> Resolution {
        if key.modifiers == Modifiers::CONTROL {
            return match key.code {
                KeyCode::Char('c') => Resolution::Action(Action::Quit),
                KeyCode::Char('[') => Resolution::Action(Action::CancelSearch),
                KeyCode::Char('n') => Resolution::Action(Action::NextMatch),
                KeyCode::Char('p') => Resolution::Action(Action::PrevMatch),
                _ => Resolution::Unbound,
            };
        }

        if key.modifiers != Modifiers::NONE {
            return Resolution::Unbound;
        }

        match key.code {
            KeyCode::Escape => Resolution::Action(Action::CancelSearch),
            KeyCode::Enter => Resolution::Action(Action::AcceptSearch),
            KeyCode::Down => Resolution::Action(Action::NextMatch),
            KeyCode::Up => Resolution::Action(Action::PrevMatch),
            _ => Resolution::Unbound,
        }
    }

    /// Filtering is an editor state: printable keys and cursor edits are
    /// forwarded, while navigation and lifecycle keys stay application-level.
    fn resolve_filter(key: KeyEvent) -> Resolution {
        if key.modifiers == Modifiers::CONTROL {
            return match key.code {
                KeyCode::Char('c') => Resolution::Action(Action::Quit),
                KeyCode::Char('[') => {
                    Resolution::Action(Action::CancelFileFilter)
                }
                KeyCode::Char('n') => {
                    Resolution::Action(Action::Move(Motion::Down(1)))
                }
                KeyCode::Char('p') => {
                    Resolution::Action(Action::Move(Motion::Up(1)))
                }
                _ => Resolution::Unbound,
            };
        }

        if key.modifiers != Modifiers::NONE {
            return Resolution::Unbound;
        }

        match key.code {
            KeyCode::Escape => Resolution::Action(Action::CancelFileFilter),
            KeyCode::Enter => Resolution::Action(Action::AcceptFileFilter),
            KeyCode::Up => Resolution::Action(Action::Move(Motion::Up(1))),
            KeyCode::Down => Resolution::Action(Action::Move(Motion::Down(1))),
            _ => Resolution::Unbound,
        }
    }

    /// While composing, only save, cancel, and the global quit chord are ours;
    /// every other key belongs to the editor widget, shifted Enter included.
    /// Escape and Ctrl+[ are the same byte on a legacy terminal but arrive as
    /// distinct events once the Kitty protocol disambiguates them, so both are
    /// bound.
    fn resolve_insert(key: KeyEvent) -> Resolution {
        match (key.code, key.modifiers) {
            (KeyCode::Escape, Modifiers::NONE) => {
                return Self::resolve_escape(Mode::Insert, false);
            }
            (KeyCode::Enter, Modifiers::NONE) => {
                return Resolution::Action(Action::CommitComment);
            }
            _ => {}
        }

        if key.modifiers != Modifiers::CONTROL {
            return Resolution::Unbound;
        }

        match key.code {
            KeyCode::Char('c') => Resolution::Action(Action::Quit),
            KeyCode::Char('[') => Self::resolve_escape(Mode::Insert, false),
            _ => Resolution::Unbound,
        }
    }
}
