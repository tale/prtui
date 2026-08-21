use termina::event::{KeyCode, KeyEvent, Modifiers};

/// Small multiline editor for draft comments. It deliberately implements only
/// terminal-native insertion and cursor movement instead of carrying a full
/// editor framework for a ten-line overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommentEditor {
    lines: Vec<String>,
    row: usize,
    column: usize,
}

impl Default for CommentEditor {
    fn default() -> Self {
        Self {
            lines: vec![String::new()],
            row: 0,
            column: 0,
        }
    }
}

impl CommentEditor {
    pub fn text(&self) -> String {
        self.lines.join("\n")
    }

    pub fn set_text(&mut self, text: impl AsRef<str>) {
        self.lines = normalized_lines(text.as_ref());
        self.row = self.lines.len().saturating_sub(1);
        self.column = self.lines[self.row].len();
    }

    pub fn lines(&self) -> &[String] {
        &self.lines
    }

    pub const fn cursor(&self) -> (usize, usize) {
        (self.row, self.column)
    }

    pub fn insert_text(&mut self, text: &str) {
        let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
        for character in normalized.chars() {
            match character {
                '\n' => self.insert_newline(),
                '\t' => self.insert_spaces(4),
                character if !character.is_control() => {
                    self.insert_char(character);
                }
                _ => {}
            }
        }
    }

    /// Returns whether the buffer or cursor changed.
    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // A terminal without the Kitty protocol reports shift+enter as plain
        // enter, which the application binds to saving. These two survive it,
        // so a multi-line comment is typeable anywhere.
        let is_newline = matches!(
            (key.code, key.modifiers),
            (KeyCode::Enter, Modifiers::ALT)
                | (KeyCode::Char('j'), Modifiers::CONTROL)
        );
        if is_newline {
            self.insert_newline();
            return true;
        }

        let command_modifiers = Modifiers::CONTROL
            | Modifiers::ALT
            | Modifiers::SUPER
            | Modifiers::HYPER
            | Modifiers::META;
        if key.modifiers.intersects(command_modifiers) {
            return false;
        }

        match key.code {
            KeyCode::Char(character) if !character.is_control() => {
                self.insert_char(character);
                true
            }
            KeyCode::Enter => {
                self.insert_newline();
                true
            }
            KeyCode::Tab => {
                self.insert_spaces(4);
                true
            }
            KeyCode::Backspace => self.backspace(),
            KeyCode::Delete => self.delete(),
            KeyCode::Left => self.left(),
            KeyCode::Right => self.right(),
            KeyCode::Up => self.vertical(-1),
            KeyCode::Down => self.vertical(1),
            KeyCode::Home => self.move_to(0),
            KeyCode::End => self.move_to(self.lines[self.row].len()),
            _ => false,
        }
    }

    fn insert_char(&mut self, character: char) {
        self.lines[self.row].insert(self.column, character);
        self.column += character.len_utf8();
    }

    fn insert_spaces(&mut self, count: usize) {
        for _ in 0..count {
            self.insert_char(' ');
        }
    }

    fn insert_newline(&mut self) {
        let tail = self.lines[self.row].split_off(self.column);
        self.row += 1;
        self.lines.insert(self.row, tail);
        self.column = 0;
    }

    fn backspace(&mut self) -> bool {
        if self.column > 0 {
            let previous =
                previous_boundary(&self.lines[self.row], self.column);
            self.lines[self.row].drain(previous..self.column);
            self.column = previous;
            return true;
        }
        if self.row == 0 {
            return false;
        }

        let current = self.lines.remove(self.row);
        self.row -= 1;
        self.column = self.lines[self.row].len();
        self.lines[self.row].push_str(&current);
        true
    }

    fn delete(&mut self) -> bool {
        if self.column < self.lines[self.row].len() {
            let next = next_boundary(&self.lines[self.row], self.column);
            self.lines[self.row].drain(self.column..next);
            return true;
        }
        if self.row + 1 == self.lines.len() {
            return false;
        }

        let next = self.lines.remove(self.row + 1);
        self.lines[self.row].push_str(&next);
        true
    }

    fn left(&mut self) -> bool {
        if self.column > 0 {
            self.column = previous_boundary(&self.lines[self.row], self.column);
            return true;
        }
        if self.row == 0 {
            return false;
        }

        self.row -= 1;
        self.column = self.lines[self.row].len();
        true
    }

    fn right(&mut self) -> bool {
        if self.column < self.lines[self.row].len() {
            self.column = next_boundary(&self.lines[self.row], self.column);
            return true;
        }
        if self.row + 1 == self.lines.len() {
            return false;
        }

        self.row += 1;
        self.column = 0;
        true
    }

    fn vertical(&mut self, direction: isize) -> bool {
        let target = self.row.saturating_add_signed(direction);
        if target >= self.lines.len() || target == self.row {
            return false;
        }

        let character_column =
            self.lines[self.row][..self.column].chars().count();
        self.row = target;
        self.column =
            byte_at_character(&self.lines[self.row], character_column);
        true
    }

    const fn move_to(&mut self, column: usize) -> bool {
        let changed = self.column != column;
        self.column = column;
        changed
    }
}

fn normalized_lines(text: &str) -> Vec<String> {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let lines: Vec<String> =
        normalized.split('\n').map(str::to_owned).collect();
    if lines.is_empty() {
        vec![String::new()]
    } else {
        lines
    }
}

fn previous_boundary(text: &str, byte: usize) -> usize {
    text[..byte]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(text: &str, byte: usize) -> usize {
    text[byte..]
        .chars()
        .next()
        .map_or(byte, |character| byte + character.len_utf8())
}

fn byte_at_character(text: &str, character: usize) -> usize {
    text.char_indices()
        .nth(character)
        .map_or(text.len(), |(byte, _)| byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn edits_unicode_on_character_boundaries() {
        let mut editor = CommentEditor::default();
        editor.insert_text("café");
        assert!(editor.handle_key(KeyCode::Left.into()));
        assert!(editor.handle_key(KeyCode::Backspace.into()));
        assert_eq!(editor.text(), "caé");
    }

    #[test]
    fn paste_normalizes_lines_and_tabs() {
        let mut editor = CommentEditor::default();
        editor.insert_text("one\r\n\ttwo");
        assert_eq!(editor.text(), "one\n    two");
        assert_eq!(editor.cursor(), (1, 7));
    }

    /// Shift+enter needs the Kitty protocol to be distinguishable from enter,
    /// which the application binds to saving.
    #[test]
    fn a_newline_is_typeable_without_the_kitty_protocol() {
        for key in [
            KeyEvent::new(KeyCode::Char('j'), Modifiers::CONTROL),
            KeyEvent::new(KeyCode::Enter, Modifiers::ALT),
        ] {
            let mut editor = CommentEditor::default();
            editor.insert_text("one");
            assert!(editor.handle_key(key));
            editor.insert_text("two");
            assert_eq!(editor.text(), "one\ntwo");
        }
    }

    #[test]
    fn backspace_joins_lines() {
        let mut editor = CommentEditor::default();
        editor.set_text("one\ntwo");
        editor.handle_key(KeyCode::Home.into());
        editor.handle_key(KeyCode::Backspace.into());
        assert_eq!(editor.text(), "onetwo");
        assert_eq!(editor.cursor(), (0, 3));
    }
}
