//! Soft wrapping for the comment editors.
//!
//! An editor buffer is a list of hard lines; the screen shows visual rows. A
//! long comment has to fold onto the next row rather than scroll sideways,
//! which means the cursor's screen position stops being a property of the
//! buffer and becomes one of the wrap — so both come from here together.

use super::measure;

/// One visual row: the buffer line it came from and the slice of it shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    pub line: usize,
    pub start: usize,
    pub end: usize,
}

pub struct Wrapped<'a> {
    lines: &'a [String],
    rows: Vec<Row>,
}

impl<'a> Wrapped<'a> {
    pub fn new(lines: &'a [String], width: usize) -> Self {
        let mut rows = Vec::with_capacity(lines.len());

        for (line, text) in lines.iter().enumerate() {
            fold(text, width, line, &mut rows);
        }

        Self { lines, rows }
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn text(&self, row: Row) -> &'a str {
        &self.lines[row.line][row.start..row.end]
    }

    /// Where a buffer position lands on screen, as a visual row and a column.
    ///
    /// A position sitting exactly on a soft break belongs to the row below it:
    /// that is where the next character typed will appear.
    pub fn locate(&self, line: usize, byte: usize) -> (usize, usize) {
        let mut last = None;

        for (index, row) in self.rows.iter().enumerate() {
            if row.line != line {
                continue;
            }
            if byte < row.end {
                return (index, self.column(*row, byte));
            }
            last = Some((index, *row));
        }

        last.map_or((0, 0), |(index, row)| (index, self.column(row, byte)))
    }

    fn column(&self, row: Row, byte: usize) -> usize {
        let text = &self.lines[row.line];
        let end = byte.clamp(row.start, text.len());

        measure::text_width(&text[row.start..end])
    }
}

fn fold(text: &str, width: usize, line: usize, rows: &mut Vec<Row>) {
    if text.is_empty() || width == 0 {
        rows.push(Row {
            line,
            start: 0,
            end: 0,
        });
        return;
    }

    let mut start = 0;
    while start < text.len() {
        let end = start + break_at(&text[start..], width);
        rows.push(Row { line, start, end });
        start = end;
    }
}

/// How many bytes of `text` fit in `width` columns.
///
/// Breaks after the last space so words stay whole, and falls back to a hard cut
/// for a word too long to ever fit. Always consumes at least one character, or
/// the caller would not terminate.
fn break_at(text: &str, width: usize) -> usize {
    let mut column = 0;
    let mut last_space = None;

    for (offset, character) in text.char_indices() {
        let character_width = measure::column_width(character, column);
        if column + character_width > width {
            return last_space
                .unwrap_or_else(|| offset.max(character.len_utf8()));
        }

        column += character_width;
        if character == ' ' {
            last_space = Some(offset + 1);
        }
    }

    text.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn buffer(lines: &[&str]) -> Vec<String> {
        lines.iter().map(|line| (*line).to_string()).collect()
    }

    fn rendered(lines: &[String], width: usize) -> Vec<String> {
        let wrapped = Wrapped::new(lines, width);

        wrapped
            .rows()
            .iter()
            .map(|row| wrapped.text(*row).to_string())
            .collect()
    }

    #[test]
    fn folds_on_spaces_and_keeps_words_whole() {
        let lines = buffer(&["the quick brown fox"]);

        assert_eq!(rendered(&lines, 10), ["the quick ", "brown fox"]);
        assert_eq!(rendered(&lines, 19), ["the quick brown fox"]);
    }

    #[test]
    fn a_word_wider_than_the_row_is_cut() {
        let lines = buffer(&["supercalifragilistic"]);

        assert_eq!(rendered(&lines, 8), ["supercal", "ifragili", "stic"]);
        // Never zero-width, or folding would not terminate.
        assert_eq!(rendered(&lines, 1).len(), 20);
    }

    #[test]
    fn hard_lines_survive_including_empty_ones() {
        let lines = buffer(&["one", "", "two"]);

        assert_eq!(rendered(&lines, 10), ["one", "", "two"]);
        assert_eq!(
            Wrapped::new(&lines, 10)
                .rows()
                .iter()
                .map(|row| row.line)
                .collect::<Vec<_>>(),
            [0, 1, 2]
        );
    }

    #[test]
    fn the_cursor_follows_the_fold() {
        let lines = buffer(&["the quick brown fox"]);
        let wrapped = Wrapped::new(&lines, 10);

        assert_eq!(wrapped.locate(0, 0), (0, 0));
        assert_eq!(wrapped.locate(0, 4), (0, 4));
        // Byte 10 is the soft break, so it reads as the start of the next row.
        assert_eq!(wrapped.locate(0, 10), (1, 0));
        assert_eq!(wrapped.locate(0, 19), (1, 9));
    }

    #[test]
    fn the_cursor_lands_on_an_empty_line() {
        let lines = buffer(&["one", "", "two"]);
        let wrapped = Wrapped::new(&lines, 10);

        assert_eq!(wrapped.locate(1, 0), (1, 0));
        assert_eq!(wrapped.locate(2, 3), (2, 3));
    }

    #[test]
    fn wide_scalars_count_columns_not_bytes() {
        let lines = buffer(&["日本語のテキスト"]);

        // Four two-column scalars fill a row of eight.
        assert_eq!(rendered(&lines, 8), ["日本語の", "テキスト"]);
        assert_eq!(Wrapped::new(&lines, 8).locate(0, 12), (1, 0));
    }
}
