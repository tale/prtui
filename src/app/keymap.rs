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

    pub(super) fn clear(&mut self) {
        self.count = None;
        self.operator = None;
    }

    pub fn resolve(&mut self, mode: Mode, filter_active: bool, key: KeyEvent) -> Resolution {
        if mode == Mode::Insert {
            return self.resolve_insert(key);
        }
        if mode == Mode::Filter {
            return self.resolve_filter(key);
        }

        let KeyCode::Char(c) = key.code else {
            return self.resolve_special(mode, filter_active, key);
        };

        if key.modifiers == Modifiers::CONTROL {
            return self.resolve_control(mode, filter_active, c);
        }

        // Shift is represented both by the character's case and, depending on
        // the terminal, by this flag. Other modifiers must not trigger a plain
        // application binding.
        if key.modifiers != Modifiers::NONE && key.modifiers != Modifiers::SHIFT {
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
            'f' => Action::ToggleTree,
            '/' if mode == Mode::Normal => Action::StartFileFilter,
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
            'q' => Action::Quit,
            _ => {
                self.clear();
                return Resolution::Unbound;
            }
        };

        self.clear();
        Resolution::Action(action)
    }

    fn resolve_control(&mut self, mode: Mode, filter_active: bool, c: char) -> Resolution {
        self.clear();

        match c {
            'c' => Resolution::Action(Action::Quit),
            'd' => Resolution::Action(Action::Move(Motion::HalfPageDown)),
            'u' => Resolution::Action(Action::Move(Motion::HalfPageUp)),
            '[' => Self::resolve_escape(mode, filter_active),
            _ => Resolution::Unbound,
        }
    }

    fn resolve_special(&mut self, mode: Mode, filter_active: bool, key: KeyEvent) -> Resolution {
        self.clear();

        if key.modifiers != Modifiers::NONE {
            return Resolution::Unbound;
        }

        match key.code {
            KeyCode::Tab => Resolution::Action(Action::TogglePane),
            KeyCode::Escape => Self::resolve_escape(mode, filter_active),
            KeyCode::Enter | KeyCode::Right if mode == Mode::Normal => {
                Resolution::Action(Action::FocusDiff)
            }
            KeyCode::Left if mode == Mode::Normal => Resolution::Action(Action::FocusFiles),
            KeyCode::Down => Resolution::Action(Action::Move(Motion::Down(1))),
            KeyCode::Up => Resolution::Action(Action::Move(Motion::Up(1))),
            _ => Resolution::Unbound,
        }
    }

    fn resolve_escape(mode: Mode, filter_active: bool) -> Resolution {
        Resolution::Action(match mode {
            Mode::Normal if filter_active => Action::ClearFileFilter,
            Mode::Normal => Action::Quit,
            Mode::Visual => Action::LeaveVisual,
            Mode::Insert => Action::CancelComment,
            Mode::Filter => Action::CancelFileFilter,
        })
    }

    /// Filtering is an editor state: printable keys and cursor edits are
    /// forwarded, while navigation and lifecycle keys stay application-level.
    fn resolve_filter(&mut self, key: KeyEvent) -> Resolution {
        if key.modifiers == Modifiers::CONTROL {
            return match key.code {
                KeyCode::Char('c') => Resolution::Action(Action::Quit),
                KeyCode::Char('[') => Resolution::Action(Action::CancelFileFilter),
                KeyCode::Char('n') => Resolution::Action(Action::Move(Motion::Down(1))),
                KeyCode::Char('p') => Resolution::Action(Action::Move(Motion::Up(1))),
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

    /// While composing, only submit, cancel, and the global quit chord are
    /// ours; every other key belongs to the editor widget. Escape and Ctrl+[
    /// are the same byte on a legacy terminal but arrive as distinct events
    /// once the Kitty protocol disambiguates them, so both are bound.
    fn resolve_insert(&mut self, key: KeyEvent) -> Resolution {
        if key.code == KeyCode::Escape && key.modifiers == Modifiers::NONE {
            return Self::resolve_escape(Mode::Insert, false);
        }

        if key.modifiers != Modifiers::CONTROL {
            return Resolution::Unbound;
        }

        match key.code {
            KeyCode::Char('s') => Resolution::Action(Action::CommitComment),
            KeyCode::Char('c') => Resolution::Action(Action::Quit),
            KeyCode::Char('[') => Self::resolve_escape(Mode::Insert, false),
            _ => Resolution::Unbound,
        }
    }
}
