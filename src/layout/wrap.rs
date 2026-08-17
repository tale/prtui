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

/// The slice of one line a single row shows, once a long line folds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fragment {
    pub start: usize,
    pub end: usize,
    /// Display column the slice begins at, so its tab stops line up with the
    /// whole line instead of restarting on every row.
    pub column: usize,
    /// Only the first row of a line carries the line numbers and the sigil.
    pub is_first: bool,
}

impl Fragment {
    pub const fn whole(text: &str) -> Self {
        Self {
            start: 0,
            end: text.len(),
            column: 0,
            is_first: true,
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.start == self.end
    }
}

/// The slices of `text` that each fit `width` columns.
///
/// Code breaks anywhere rather than on spaces: an identifier reads better split
/// at the edge than moved whole onto the next row, and columns stay aligned with
/// the line above.
pub fn fragments(text: &str, width: usize) -> Vec<Fragment> {
    // Without a tab, a scalar never occupies more columns than it does bytes, so
    // a line this short provably fits and never needs measuring.
    if width == 0 || (!text.contains('\t') && text.len() <= width) {
        return vec![Fragment::whole(text)];
    }

    let mut fragments = Vec::new();
    let mut start = 0;
    let mut column = 0;

    while start < text.len() {
        let mut used = 0;
        let mut end = start;

        for (offset, character) in text[start..].char_indices() {
            let character_width =
                measure::column_width(character, column + used);
            if used + character_width > width {
                break;
            }
            used += character_width;
            end = start + offset + character.len_utf8();
        }

        // A single scalar wider than the whole row still has to advance, or the
        // fold would not terminate.
        if end == start {
            end =
                start + text[start..].chars().next().map_or(1, char::len_utf8);
        }

        fragments.push(Fragment {
            start,
            end,
            column,
            is_first: start == 0,
        });
        column += used;
        start = end;
    }

    fragments
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

#[cfg(test)]
mod fragment_tests {
    use super::*;

    fn slices(text: &str, width: usize) -> Vec<&str> {
        fragments(text, width)
            .into_iter()
            .map(|fragment| &text[fragment.start..fragment.end])
            .collect()
    }

    #[test]
    fn a_short_line_is_one_fragment() {
        assert_eq!(slices("fn main() {}", 40), ["fn main() {}"]);
        assert_eq!(slices("", 40), [""]);
        assert!(fragments("", 40)[0].is_empty());
    }

    #[test]
    fn code_breaks_at_the_edge_rather_than_on_spaces() {
        // Prose wrapping would have moved `world` down whole.
        assert_eq!(slices("hello world", 8), ["hello wo", "rld"]);
    }

    #[test]
    fn continuations_report_where_they_start() {
        let folded = fragments("abcdefghij", 4);

        assert_eq!(folded.len(), 3);
        assert!(folded[0].is_first);
        assert!(!folded[1].is_first);
        assert_eq!(
            folded.iter().map(|f| f.column).collect::<Vec<_>>(),
            [0, 4, 8]
        );
    }

    #[test]
    fn tabs_keep_their_stops_across_a_fold() {
        // The tab fills to column 4, so only four more columns fit on row one.
        assert_eq!(slices("\tabcdefgh", 8), ["\tabcd", "efgh"]);

        // A continuation resumes the column count rather than restarting it, so
        // a tab inside it still lands on a real stop.
        let folded = fragments("ab\tcd\tefgh", 6);
        assert_eq!(folded[0].column, 0);
        assert_eq!(folded[1].column, 6);
    }

    #[test]
    fn wide_scalars_never_split_and_always_advance() {
        assert_eq!(slices("日本語", 4), ["日本", "語"]);
        // Narrower than one scalar: it takes the row alone instead of looping.
        assert_eq!(slices("日本", 1), ["日", "本"]);
    }

    #[test]
    fn every_byte_survives_the_fold() {
        let line =
            "let x = compute(alpha, beta, gamma) + \tdelta_epsilon_zeta;";
        let rejoined: String = slices(line, 11).concat();

        assert_eq!(rejoined, line, "folding must not drop content");
    }
}
