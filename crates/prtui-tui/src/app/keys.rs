use std::fmt;
use termina::event::{KeyCode, KeyEvent, Modifiers};

/// One keypress, normalized so a binding can be matched by value.
///
/// Shift is folded into the character it produced. A terminal reports it as the
/// case of the character, as the modifier flag, or as both, depending on the
/// keyboard protocol in use, and a binding table cannot carry three spellings
/// of the same key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub code: KeyCode,
    pub modifiers: Modifiers,
}

impl Key {
    pub fn new(code: KeyCode, modifiers: Modifiers) -> Self {
        let (code, modifiers) = match (code, modifiers) {
            // The same byte on a legacy terminal, and the Kitty protocol only
            // tells them apart to hand back a distinction nothing wants.
            (KeyCode::Char('['), mods) if mods.contains(Modifiers::CONTROL) => {
                (KeyCode::Escape, mods - Modifiers::CONTROL)
            }
            (KeyCode::Char(character), mods)
                if mods.contains(Modifiers::SHIFT) =>
            {
                (
                    KeyCode::Char(character.to_ascii_uppercase()),
                    mods - Modifiers::SHIFT,
                )
            }
            (KeyCode::Tab, mods) if mods.contains(Modifiers::SHIFT) => {
                (KeyCode::BackTab, mods - Modifiers::SHIFT)
            }
            (KeyCode::BackTab, mods) => {
                (KeyCode::BackTab, mods - Modifiers::SHIFT)
            }
            _ => (code, modifiers),
        };

        Self { code, modifiers }
    }

    pub fn from_event(event: KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }

    /// The character this key stands for, when it carries no modifier. This is
    /// what count digits are read from.
    pub const fn as_char(self) -> Option<char> {
        match self.code {
            KeyCode::Char(character) if self.modifiers.is_empty() => {
                Some(character)
            }
            _ => None,
        }
    }
}

/// Parses a chord in Vim's notation — `gg`, `<C-d>`, `]c`, `<S-Tab>` — into the
/// keys that have to arrive in order for it to fire.
pub fn chord(text: &str) -> Option<Vec<Key>> {
    let mut keys = Vec::new();
    let mut rest = text;

    while let Some(first) = rest.chars().next() {
        if first != '<' {
            keys.push(Key::new(KeyCode::Char(first), Modifiers::NONE));
            rest = &rest[first.len_utf8()..];
            continue;
        }

        let close = rest.find('>')?;
        keys.push(named(&rest[1..close])?);
        rest = &rest[close + 1..];
    }

    (!keys.is_empty()).then_some(keys)
}

/// The inside of a `<...>`: any number of modifier letters, then a key name.
fn named(text: &str) -> Option<Key> {
    let mut modifiers = Modifiers::NONE;
    let mut rest = text;

    while let Some((prefix, tail)) = rest.split_once('-') {
        let modifier = match prefix.to_ascii_lowercase().as_str() {
            "c" => Modifiers::CONTROL,
            "s" => Modifiers::SHIFT,
            "a" | "m" => Modifiers::ALT,
            "d" => Modifiers::SUPER,
            _ => break,
        };
        modifiers |= modifier;
        rest = tail;
    }

    Some(Key::new(code(rest)?, modifiers))
}

fn code(name: &str) -> Option<KeyCode> {
    let mut characters = name.chars();
    if let (Some(single), None) = (characters.next(), characters.next()) {
        return Some(KeyCode::Char(single));
    }

    let lower = name.to_ascii_lowercase();
    if let Some(number) = lower
        .strip_prefix('f')
        .and_then(|digits| digits.parse().ok())
    {
        return Some(KeyCode::Function(number));
    }

    Some(match lower.as_str() {
        "cr" | "enter" | "return" => KeyCode::Enter,
        "esc" | "escape" => KeyCode::Escape,
        "tab" => KeyCode::Tab,
        "space" => KeyCode::Char(' '),
        "bs" | "backspace" => KeyCode::Backspace,
        "del" | "delete" => KeyCode::Delete,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "lt" => KeyCode::Char('<'),
        "gt" => KeyCode::Char('>'),
        "bar" => KeyCode::Char('|'),
        _ => return None,
    })
}

impl fmt::Display for Key {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(character) = self.as_char()
            && character != ' '
        {
            return write!(formatter, "{character}");
        }

        let name = match self.code {
            KeyCode::Char(' ') => "Space".to_owned(),
            KeyCode::Char(character) => character.to_string(),
            KeyCode::Enter => "CR".to_owned(),
            KeyCode::Escape => "Esc".to_owned(),
            KeyCode::Tab => "Tab".to_owned(),
            KeyCode::BackTab => "S-Tab".to_owned(),
            KeyCode::Backspace => "BS".to_owned(),
            KeyCode::Delete => "Del".to_owned(),
            KeyCode::Up => "Up".to_owned(),
            KeyCode::Down => "Down".to_owned(),
            KeyCode::Left => "Left".to_owned(),
            KeyCode::Right => "Right".to_owned(),
            KeyCode::Home => "Home".to_owned(),
            KeyCode::End => "End".to_owned(),
            KeyCode::PageUp => "PageUp".to_owned(),
            KeyCode::PageDown => "PageDown".to_owned(),
            KeyCode::Function(number) => format!("F{number}"),
            other => format!("{other:?}"),
        };

        let mut prefix = String::new();
        if self.modifiers.contains(Modifiers::CONTROL) {
            prefix.push_str("C-");
        }
        if self.modifiers.contains(Modifiers::ALT) {
            prefix.push_str("A-");
        }
        if self.modifiers.contains(Modifiers::SUPER) {
            prefix.push_str("D-");
        }

        write!(formatter, "<{prefix}{name}>")
    }
}

/// The chord as it would be written in a keymap, for the pending-command hint.
pub fn render(keys: &[Key]) -> String {
    keys.iter().map(Key::to_string).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_chord_is_one_key_per_notation_token() {
        assert_eq!(chord("gg").unwrap().len(), 2);
        assert_eq!(chord("<C-w>h").unwrap().len(), 2);
        assert!(chord("<C-d").is_none());
        assert!(chord("<nope>").is_none());
        assert!(chord("").is_none());
    }

    /// The same physical key reaches us three ways, and all three have to land
    /// on the binding written once in the table.
    #[test]
    fn shift_folds_into_the_character_it_produced() {
        let written = chord("G").unwrap()[0];

        for reported in [
            Key::new(KeyCode::Char('G'), Modifiers::NONE),
            Key::new(KeyCode::Char('G'), Modifiers::SHIFT),
            Key::new(KeyCode::Char('g'), Modifiers::SHIFT),
        ] {
            assert_eq!(reported, written);
        }

        assert_eq!(chord("<S-a>").unwrap()[0], chord("A").unwrap()[0]);
    }

    /// Every mode binds escape, and half the terminals in use send Ctrl+[ for
    /// it. Folding them here is what keeps that out of the binding table.
    #[test]
    fn ctrl_bracket_is_escape() {
        assert_eq!(
            Key::new(KeyCode::Char('['), Modifiers::CONTROL),
            chord("<Esc>").unwrap()[0]
        );
    }

    /// A terminal sends shift+tab as either code, and the submit form binds it.
    #[test]
    fn shift_tab_is_one_key_however_it_arrives() {
        let written = chord("<S-Tab>").unwrap()[0];

        assert_eq!(Key::new(KeyCode::BackTab, Modifiers::SHIFT), written);
        assert_eq!(Key::new(KeyCode::BackTab, Modifiers::NONE), written);
        assert_eq!(Key::new(KeyCode::Tab, Modifiers::SHIFT), written);
    }

    #[test]
    fn notation_round_trips() {
        for text in ["j", "<C-d>", "<Esc>", "<CR>", "<Space>", "<F5>", "<A-x>"]
        {
            assert_eq!(render(&chord(text).unwrap()), text);
        }
    }
}
