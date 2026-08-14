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

    pub fn resolve(&mut self, mode: Mode, key: KeyEvent) -> Resolution {
        if mode == Mode::Insert {
            return self.resolve_insert(key);
        }

        let KeyCode::Char(c) = key.code else {
            return self.resolve_special(key);
        };

        if key.modifiers == Modifiers::CONTROL {
            return self.resolve_control(c);
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

    fn resolve_control(&mut self, c: char) -> Resolution {
        self.clear();

        match c {
            'd' => Resolution::Action(Action::Move(Motion::HalfPageDown)),
            'u' => Resolution::Action(Action::Move(Motion::HalfPageUp)),
            _ => Resolution::Unbound,
        }
    }

    fn resolve_special(&mut self, key: KeyEvent) -> Resolution {
        self.clear();

        if key.modifiers != Modifiers::NONE {
            return Resolution::Unbound;
        }

        match key.code {
            KeyCode::Tab => Resolution::Action(Action::TogglePane),
            KeyCode::Escape => Resolution::Action(Action::LeaveVisual),
            KeyCode::Down => Resolution::Action(Action::Move(Motion::Down(1))),
            KeyCode::Up => Resolution::Action(Action::Move(Motion::Up(1))),
            _ => Resolution::Unbound,
        }
    }

    /// While composing, only the submit and cancel chords are ours; every
    /// other key belongs to the editor widget.
    fn resolve_insert(&mut self, key: KeyEvent) -> Resolution {
        if key.modifiers != Modifiers::CONTROL {
            return Resolution::Unbound;
        }

        match key.code {
            KeyCode::Char('s') => Resolution::Action(Action::CommitComment),
            KeyCode::Char('c') => Resolution::Action(Action::CancelComment),
            _ => Resolution::Unbound,
        }
    }
}
