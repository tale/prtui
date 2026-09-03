use termina::event::{KeyCode, KeyEvent, Modifiers};

/// A readline edit.
///
/// Readline is the standard here rather than Vim: these are the chords bash,
/// zsh, and every other terminal prompt answer to, and a prompt is a terminal
/// prompt wherever it is drawn. A word is readline's own — a run of letters
/// and digits — except in `DeleteToBlank`, which is Ctrl+W's, delimited by
/// whitespace alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Edit {
    LineStart,
    LineEnd,
    CharLeft,
    CharRight,
    WordLeft,
    WordRight,
    DeleteChar,
    DeleteWordLeft,
    DeleteWordRight,
    DeleteToBlank,
    DeleteToStart,
    DeleteToEnd,
}

impl Edit {
    /// Whether the edit changes the text rather than only where the cursor
    /// sits. Moving around inside a recalled line does not move off it.
    pub const fn is_destructive(self) -> bool {
        matches!(
            self,
            Self::DeleteChar
                | Self::DeleteWordLeft
                | Self::DeleteWordRight
                | Self::DeleteToBlank
                | Self::DeleteToStart
                | Self::DeleteToEnd
        )
    }
}

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

    /// Joins the buffer and trims it in place, retaining the join allocation.
    pub fn trimmed_text(&self) -> String {
        let mut text = self.text();
        let (start, end) = {
            let trimmed = text.trim();
            let start = text.len() - text.trim_start().len();
            (start, start + trimmed.len())
        };

        if start == end {
            text.clear();
            return text;
        }

        text.truncate(end);
        text.drain(..start);
        text
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

    /// Runs a readline edit. Returns whether the buffer or cursor changed.
    ///
    /// Every motion here stays on the row it started on: the ends of the line
    /// are what the chords name, and the arrows already cross rows.
    pub fn edit(&mut self, edit: Edit) -> bool {
        let line = &self.lines[self.row];
        let target = match edit {
            Edit::CharLeft => return self.left(),
            Edit::CharRight => return self.right(),
            Edit::DeleteChar => return self.delete(),
            Edit::LineStart | Edit::DeleteToStart => 0,
            Edit::LineEnd | Edit::DeleteToEnd => line.len(),
            Edit::WordLeft | Edit::DeleteWordLeft => {
                word_start(line, self.column)
            }
            Edit::WordRight | Edit::DeleteWordRight => {
                word_end(line, self.column)
            }
            Edit::DeleteToBlank => blank_word_start(line, self.column),
        };

        if edit.is_destructive() {
            return self.cut_to(target);
        }

        self.move_to(target)
    }

    /// Drops the text between the cursor and a byte on the same line.
    fn cut_to(&mut self, target: usize) -> bool {
        if target == self.column {
            return false;
        }

        let start = target.min(self.column);
        let end = target.max(self.column);
        self.lines[self.row].drain(start..end);
        self.column = start;
        true
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

/// Readline's word: a run of letters and digits, whatever separates them.
fn word_start(text: &str, byte: usize) -> usize {
    text[..byte]
        .trim_end_matches(|character: char| !character.is_alphanumeric())
        .trim_end_matches(char::is_alphanumeric)
        .len()
}

fn word_end(text: &str, byte: usize) -> usize {
    let rest = text[byte..]
        .trim_start_matches(|character: char| !character.is_alphanumeric())
        .trim_start_matches(char::is_alphanumeric);

    text.len() - rest.len()
}

/// Ctrl+W's word, which the shell delimits by whitespace alone, so one press
/// takes back a whole path rather than the last name in it.
fn blank_word_start(text: &str, byte: usize) -> usize {
    text[..byte]
        .trim_end_matches(char::is_whitespace)
        .trim_end_matches(|character: char| !character.is_whitespace())
        .len()
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

    fn typed(text: &str) -> CommentEditor {
        let mut editor = CommentEditor::default();
        editor.insert_text(text);
        editor
    }

    #[test]
    fn readline_chords_walk_to_the_ends_of_the_line() {
        let mut editor = typed("one two");
        assert!(editor.edit(Edit::LineStart));
        assert_eq!(editor.cursor(), (0, 0));
        assert!(!editor.edit(Edit::LineStart));

        assert!(editor.edit(Edit::LineEnd));
        assert_eq!(editor.cursor(), (0, 7));

        editor.insert_text("\nthree");
        editor.edit(Edit::LineStart);
        assert_eq!(editor.cursor(), (1, 0));
    }

    /// A word motion steps over what separates words before it steps over the
    /// word, the way readline's does.
    #[test]
    fn a_word_motion_lands_between_words() {
        let mut editor = typed("one two.three");
        editor.edit(Edit::WordLeft);
        assert_eq!(editor.cursor(), (0, 8));
        editor.edit(Edit::WordLeft);
        assert_eq!(editor.cursor(), (0, 4));

        editor.edit(Edit::WordRight);
        assert_eq!(editor.cursor(), (0, 7));
        editor.edit(Edit::WordRight);
        assert_eq!(editor.cursor(), (0, 13));
    }

    /// Ctrl+W is whitespace-delimited where the alt chords are not, so it
    /// takes back a whole path and Alt+Backspace takes the name in it.
    #[test]
    fn the_kills_cut_what_their_chords_name() {
        let mut editor = typed("look at src/app/editor.rs");
        assert!(editor.edit(Edit::DeleteToBlank));
        assert_eq!(editor.text(), "look at ");

        let mut editor = typed("look at src/app/editor.rs");
        editor.edit(Edit::DeleteWordLeft);
        assert_eq!(editor.text(), "look at src/app/editor.");

        let mut editor = typed("look at src/app/editor.rs");
        editor.edit(Edit::DeleteToStart);
        assert_eq!(editor.text(), "");
        assert_eq!(editor.cursor(), (0, 0));

        let mut editor = typed("one two");
        editor.edit(Edit::LineStart);
        editor.edit(Edit::DeleteWordRight);
        assert_eq!(editor.text(), " two");
        editor.edit(Edit::DeleteToEnd);
        assert_eq!(editor.text(), "");
    }

    /// The kills stay on their own row: a comment is several lines and the
    /// chord names the line the cursor is on.
    #[test]
    fn a_kill_leaves_the_neighbouring_lines_alone() {
        let mut editor = typed("one\ntwo\nthree");
        editor.edit(Edit::LineStart);
        editor.edit(Edit::DeleteToEnd);
        assert_eq!(editor.text(), "one\ntwo\n");
        assert!(!editor.edit(Edit::DeleteToEnd));
    }

    #[test]
    fn a_kill_cuts_on_character_boundaries() {
        let mut editor = typed("café ☕");
        editor.edit(Edit::DeleteToBlank);
        assert_eq!(editor.text(), "café ");
        editor.edit(Edit::DeleteToBlank);
        assert_eq!(editor.text(), "");
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
