use std::borrow::Cow;
use unicode_width::UnicodeWidthChar;

const TAB: usize = 4;

pub fn column_width(character: char, column: usize) -> usize {
    if character == '\t' {
        TAB - (column % TAB)
    } else {
        UnicodeWidthChar::width(character).unwrap_or(0)
    }
}

pub fn text_width(text: &str) -> usize {
    text.chars().fold(0, |column, character| {
        column + column_width(character, column)
    })
}

pub fn clip_text_to_budget(
    text: &str,
    budget: usize,
    column: usize,
) -> (Cow<'_, str>, usize) {
    // Only a tab makes the drawn text differ from the source bytes. Everything
    // else clips to a slice of the caller's own string, which a `Span` then
    // borrows instead of copying — `column` matters solely for tab stops.
    if !text.contains('\t') {
        let (end, used) = fitting_prefix(text, budget);
        return (Cow::Borrowed(&text[..end]), used);
    }

    let mut rendered = String::with_capacity(text.len().min(budget));
    let mut used = 0;
    for character in text.chars() {
        let character_width = column_width(character, column + used);
        if used + character_width > budget {
            break;
        }

        if character == '\t' {
            rendered.extend(std::iter::repeat_n(' ', character_width));
        } else {
            rendered.push(character);
        }
        used += character_width;
    }

    (Cow::Owned(rendered), used)
}

/// Byte offset where `text` exhausts `budget` columns, and the columns up to it.
/// A character that would straddle the edge is left out rather than half-drawn.
fn fitting_prefix(text: &str, budget: usize) -> (usize, usize) {
    let mut used = 0;

    for (offset, character) in text.char_indices() {
        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > budget {
            return (offset, used);
        }
        used += character_width;
    }

    (text.len(), used)
}

/// The `budget` columns of `text` starting at column `start`, for horizontally
/// scrolled single-line fields. Characters straddling either edge are dropped
/// rather than half-drawn.
pub fn window(text: &str, start: usize, budget: usize) -> String {
    let mut rendered = String::with_capacity(text.len().min(budget));
    let end = start.saturating_add(budget);
    let mut column = 0;

    for character in text.chars() {
        let character_width = column_width(character, column);
        let next = column + character_width;

        if next <= start {
            column = next;
            continue;
        }
        if column < start || next > end {
            column = next;
            if column >= end {
                break;
            }
            continue;
        }

        if character == '\t' {
            rendered.extend(std::iter::repeat_n(' ', character_width));
        } else {
            rendered.push(character);
        }
        column = next;
    }

    rendered
}

/// Cut `text` to `budget` columns, marking the cut with an ellipsis.
pub fn truncate(text: &str, budget: usize) -> String {
    if text.chars().count() <= budget {
        return text.to_string();
    }

    let head: String = text.chars().take(budget.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Splits `a/b/c.rs` into a dimmed `a/b/` and a bright `c.rs`, dropping leading
/// directories when the whole path will not fit.
///
/// The name is what identifies a file and the directory just above it is what
/// tells two files of the same name apart, so the cut comes off the front and
/// lands on a separator. Cutting mid-segment instead used to leave `…fy/` in
/// front of the name, which names no directory that exists.
pub fn split_path(path: &str, budget: usize) -> (String, String) {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();

    if name.chars().count() >= budget {
        let tail: String = name
            .chars()
            .skip(name.chars().count() + 1 - budget)
            .collect();
        return (String::new(), format!("…{tail}"));
    }

    let directory_budget = budget - name.chars().count();
    let directory = path.strip_suffix(&name).unwrap_or("");

    if directory.chars().count() <= directory_budget {
        return (directory.to_string(), name);
    }

    // Whole segments off the end, longest first, with one column held back for
    // the ellipsis. The last separator always yields `…/`, which still says the
    // file is nested when nothing else fits.
    let kept_budget = directory_budget.saturating_sub(1);
    let kept = directory
        .match_indices('/')
        .map(|(at, _)| &directory[at..])
        .find(|tail| tail.chars().count() <= kept_budget);

    match kept {
        Some(tail) => (format!("…{tail}"), name),
        None => (String::new(), name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measures_tabs_against_their_stops() {
        assert_eq!(text_width("\t"), 4);
        assert_eq!(text_width("ab\t"), 4);
        assert_eq!(text_width("abcd\t"), 8);
        // A wide scalar counts two columns, not one.
        assert_eq!(text_width("日本"), 4);
    }

    #[test]
    fn clip_expands_tabs_from_the_starting_column() {
        assert_eq!(clip_text_to_budget("\tx", 8, 0), ("    x".into(), 5));
        // Starting two columns in leaves a two-column tab.
        assert_eq!(clip_text_to_budget("\tx", 8, 2), ("  x".into(), 3));
        // A character that does not fit whole is dropped entirely.
        assert_eq!(clip_text_to_budget("日本", 3, 0), ("日".into(), 2));
        assert_eq!(clip_text_to_budget("anything", 0, 0), ("".into(), 0));
    }

    #[test]
    fn window_drops_characters_straddling_either_edge() {
        assert_eq!(window("abcdef", 2, 3), "cde");
        assert_eq!(window("abc", 0, 10), "abc");
        // The wide scalar spans columns 0-1, so a window opening at 1 skips it.
        assert_eq!(window("日x", 1, 2), "x");
    }

    #[test]
    fn truncate_marks_where_it_cut() {
        assert_eq!(truncate("abcdef", 6), "abcdef");
        assert_eq!(truncate("abcdef", 4), "abc…");
    }

    #[test]
    fn split_path_elides_the_directory_before_the_name() {
        assert_eq!(
            split_path("src/app/mod.rs", 20),
            ("src/app/".into(), "mod.rs".into())
        );
        // The nearest directory is what disambiguates, so it survives whole.
        assert_eq!(
            split_path("pkg/cmd/attestation/verify/verify.go", 18),
            ("…/verify/".into(), "verify.go".into())
        );
        // No segment fits, so the path says only that it is nested.
        assert_eq!(
            split_path("src/app/mod.rs", 10),
            ("…/".into(), "mod.rs".into())
        );
        // Too narrow for the name itself: only its tail survives.
        assert_eq!(
            split_path("src/app/mod.rs", 4),
            (String::new(), "….rs".into())
        );
    }
}
