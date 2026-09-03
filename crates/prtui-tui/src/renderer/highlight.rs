use super::ThemeMode;
use prtui_core::{DiffLine, LineKind};
use similar::{ChangeTag, TextDiff};
use std::ops::Range;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    Color, ScopeSelectors, StyleModifier, Theme, ThemeItem, ThemeSettings,
};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// A styled byte range into its source [`DiffLine`]. Keeping ranges instead of
/// copied strings makes the highlight cache small even for very large diffs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Segment {
    pub range: Range<usize>,
    pub color: (u8, u8, u8),
    pub is_emphasis: bool,
}

struct Assets {
    syntaxes: SyntaxSet,
    dark: Theme,
    light: Theme,
}

impl Assets {
    fn syntax_for(&self, path: &str, lines: &[DiffLine]) -> &SyntaxReference {
        let file_name = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(path);
        let extension = std::path::Path::new(path)
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default();

        self.syntaxes
            .find_syntax_by_extension(file_name)
            .or_else(|| self.syntaxes.find_syntax_by_extension(extension))
            .or_else(|| {
                lines
                    .iter()
                    .find(|line| line.kind != LineKind::Hunk)
                    .and_then(|line| {
                        self.syntaxes.find_syntax_by_first_line(&line.text)
                    })
            })
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    const fn theme(&self, mode: ThemeMode) -> &Theme {
        match mode {
            ThemeMode::Dark => &self.dark,
            ThemeMode::Light => &self.light,
        }
    }
}

static ASSETS: OnceLock<Assets> = OnceLock::new();

const fn syntax_color(rgb: u32) -> Color {
    Color {
        r: ((rgb >> 16) & 0xff) as u8,
        g: ((rgb >> 8) & 0xff) as u8,
        b: (rgb & 0xff) as u8,
        a: 255,
    }
}

fn theme_item(scopes: &str, rgb: u32) -> ThemeItem {
    ThemeItem {
        scope: scopes
            .parse::<ScopeSelectors>()
            .expect("static scope selectors are valid"),
        style: StyleModifier {
            foreground: Some(syntax_color(rgb)),
            ..StyleModifier::default()
        },
    }
}

/// GitHub Light/Dark Default's semantic syntax palette, expressed as syntect
/// scopes. The colors and token groupings follow Primer's official themes.
fn github_theme(mode: ThemeMode) -> Theme {
    let (
        foreground,
        background,
        comment,
        constant,
        string,
        entity,
        function,
        tag,
        keyword,
    ) = match mode {
        ThemeMode::Dark => (
            0xe6edf3, 0x0d1117, 0x8c959f, 0x79c0ff, 0xa5d6ff, 0xffa657,
            0xd2a8ff, 0x7ee787, 0xff7b72,
        ),
        ThemeMode::Light => (
            0x1f2328, 0xffffff, 0x6e7781, 0x0550ae, 0x0a3069, 0x953800,
            0x8250df, 0x116329, 0xcf222e,
        ),
    };

    Theme {
        name: Some(
            match mode {
                ThemeMode::Dark => "GitHub Dark Default",
                ThemeMode::Light => "GitHub Light Default",
            }
            .into(),
        ),
        author: Some("GitHub / Primer".into()),
        settings: ThemeSettings {
            foreground: Some(syntax_color(foreground)),
            background: Some(syntax_color(background)),
            ..ThemeSettings::default()
        },
        // Broad rules come first; later, more specific selectors win.
        scopes: vec![
            theme_item("comment, punctuation.definition.comment", comment),
            theme_item(
                "constant, support.constant, meta.module-reference",
                constant,
            ),
            theme_item("constant.numeric", constant),
            theme_item("string, punctuation.definition.string", string),
            theme_item(
                "entity.name.type, entity.name.class, entity.name.namespace, entity.other.inherited-class, support.type",
                entity,
            ),
            theme_item("entity.name.function, support.function", function),
            theme_item("entity.name.tag", tag),
            theme_item("keyword, storage, storage.type", keyword),
            theme_item("variable, entity.name.variable", entity),
            theme_item("invalid, invalid.illegal", keyword),
            theme_item("markup.heading", constant),
            theme_item("markup.inserted", tag),
            theme_item("markup.deleted", keyword),
        ],
    }
}

fn assets() -> &'static Assets {
    ASSETS.get_or_init(|| Assets {
        syntaxes: two_face::syntax::extra_newlines(),
        dark: github_theme(ThemeMode::Dark),
        light: github_theme(ThemeMode::Light),
    })
}

pub fn preload(mode: ThemeMode) {
    let _ = assets().theme(mode);
}

/// Splits on identifier boundaries rather than whitespace, so `compute(alpha,`
/// becomes `compute` `(` `alpha` `,` and only the argument gets marked.
fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let first = rest.chars().next().expect("rest is not empty");

        let take = if is_word(first) {
            rest.find(|c: char| !is_word(c)).unwrap_or(rest.len())
        } else if first.is_whitespace() {
            rest.find(|c: char| !c.is_whitespace())
                .unwrap_or(rest.len())
        } else {
            first.len_utf8()
        };

        let (token, tail) = rest.split_at(take);
        tokens.push(token);
        rest = tail;
    }

    tokens
}

fn push_range(ranges: &mut Vec<Range<usize>>, range: Range<usize>) {
    if range.is_empty() {
        return;
    }
    if let Some(last) = ranges.last_mut()
        && last.end == range.start
    {
        last.end = range.end;
        return;
    }
    ranges.push(range);
}

/// Rejoins runs that a common space split apart.
///
/// Whitespace tokens match across almost any pair of lines, so two changed
/// words with a space between them come back as two marks with a hole punched
/// between them. On a line where every word changed, that hole appears between
/// each pair and the line renders as a checkerboard.
fn join_across_whitespace(
    text: &str,
    ranges: Vec<Range<usize>>,
) -> Vec<Range<usize>> {
    let mut joined: Vec<Range<usize>> = Vec::with_capacity(ranges.len());

    for range in ranges {
        let is_gap_blank = joined.last().is_some_and(|last| {
            text.get(last.end..range.start)
                .is_some_and(|gap| gap.chars().all(char::is_whitespace))
        });

        match joined.last_mut() {
            Some(last) if is_gap_blank => last.end = range.end,
            _ => joined.push(range),
        }
    }

    joined
}

/// Whether the marks cover the line rather than picking things out of it.
fn is_rewritten(text: &str, ranges: &[Range<usize>]) -> bool {
    let marked: usize = ranges.iter().map(ExactSizeIterator::len).sum();
    let content = text.trim().len();

    content > 0 && marked * 10 >= content * 9
}

/// Byte ranges of the tokens that changed between a removed and added line.
fn emphasis_ranges(
    old: &str,
    new: &str,
) -> (Vec<Range<usize>>, Vec<Range<usize>>) {
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    let diff = TextDiff::from_slices(&old_tokens, &new_tokens);

    let mut old_ranges = Vec::new();
    let mut new_ranges = Vec::new();
    let mut old_cursor = 0;
    let mut new_cursor = 0;

    for change in diff.iter_all_changes() {
        let len = change.value().len();
        match change.tag() {
            ChangeTag::Equal => {
                old_cursor += len;
                new_cursor += len;
            }
            ChangeTag::Delete => {
                push_range(&mut old_ranges, old_cursor..old_cursor + len);
                old_cursor += len;
            }
            ChangeTag::Insert => {
                push_range(&mut new_ranges, new_cursor..new_cursor + len);
                new_cursor += len;
            }
        }
    }

    (
        join_across_whitespace(old, old_ranges),
        join_across_whitespace(new, new_ranges),
    )
}

/// Pairs balanced removed/added runs. Unbalanced runs remain fully tinted;
/// inventing a line pairing there tends to emphasize unrelated code.
fn build_emphasis(lines: &[DiffLine]) -> Vec<Option<Vec<Range<usize>>>> {
    let mut emphasis = vec![None; lines.len()];

    let mut index = 0;
    while index < lines.len() {
        if lines[index].kind != LineKind::Removed {
            index += 1;
            continue;
        }

        let removed_start = index;
        while index < lines.len() && lines[index].kind == LineKind::Removed {
            index += 1;
        }

        let added_start = index;
        while index < lines.len() && lines[index].kind == LineKind::Added {
            index += 1;
        }

        let removed = removed_start..added_start;
        let added = added_start..index;
        if removed.len() != added.len() {
            continue;
        }

        for (old_index, new_index) in removed.zip(added) {
            let old_text = &lines[old_index].text;
            let new_text = &lines[new_index].text;
            let (old_ranges, new_ranges) = emphasis_ranges(old_text, new_text);

            // Two lines with nothing in common are a rewrite, not an edit.
            // Marking every token of both says what the line's own tint
            // already says, in a louder color.
            if is_rewritten(old_text, &old_ranges)
                || is_rewritten(new_text, &new_ranges)
            {
                continue;
            }

            emphasis[old_index] = Some(old_ranges);
            emphasis[new_index] = Some(new_ranges);
        }
    }

    emphasis
}

fn push_segment(segments: &mut Vec<Segment>, segment: Segment) {
    if segment.range.is_empty() {
        return;
    }
    if let Some(last) = segments.last_mut()
        && last.range.end == segment.range.start
        && last.color == segment.color
        && last.is_emphasis == segment.is_emphasis
    {
        last.range.end = segment.range.end;
        return;
    }
    segments.push(segment);
}

fn split_region(
    segments: &mut Vec<Segment>,
    region: Range<usize>,
    color: (u8, u8, u8),
    emphasis: &[Range<usize>],
    emphasis_index: &mut usize,
) {
    let mut cursor = region.start;
    while *emphasis_index < emphasis.len()
        && emphasis[*emphasis_index].end <= cursor
    {
        *emphasis_index += 1;
    }

    while cursor < region.end {
        let changed = emphasis.get(*emphasis_index);
        let is_emphasis = changed
            .is_some_and(|range| range.start <= cursor && cursor < range.end);
        let end = match changed {
            Some(range) if is_emphasis => region.end.min(range.end),
            Some(range) => region.end.min(range.start),
            None => region.end,
        };

        push_segment(
            segments,
            Segment {
                range: cursor..end,
                color,
                is_emphasis,
            },
        );
        cursor = end;

        if changed.is_some_and(|range| cursor >= range.end) {
            *emphasis_index += 1;
        }
    }
}

const MAX_HIGHLIGHT_BYTES: usize = 16 * 1024;

pub fn highlight_file(
    path: &str,
    lines: &[DiffLine],
    mode: ThemeMode,
) -> Vec<Vec<Segment>> {
    let assets = assets();
    let syntax = assets.syntax_for(path, lines);
    let syntax_theme = assets.theme(mode);
    let fallback = syntax_theme.settings.foreground.unwrap_or(match mode {
        ThemeMode::Dark => syntax_color(0xe6edf3),
        ThemeMode::Light => syntax_color(0x1f2328),
    });

    // Each side keeps parser state independently. A hunk boundary resets both:
    // omitted source between hunks cannot safely carry lexical state forward.
    let mut old_side = HighlightLines::new(syntax, syntax_theme);
    let mut new_side = HighlightLines::new(syntax, syntax_theme);
    let emphasis = build_emphasis(lines);
    let mut line_buffer = String::new();

    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if line.kind == LineKind::Hunk {
                old_side = HighlightLines::new(syntax, syntax_theme);
                new_side = HighlightLines::new(syntax, syntax_theme);
                return Vec::new();
            }

            // Pathological generated/minified lines can monopolize a regex
            // worker. They are still rendered completely, just as plain text.
            if line.text.len() > MAX_HIGHLIGHT_BYTES {
                old_side = HighlightLines::new(syntax, syntax_theme);
                new_side = HighlightLines::new(syntax, syntax_theme);
                return vec![Segment {
                    range: 0..line.text.len(),
                    color: (fallback.r, fallback.g, fallback.b),
                    is_emphasis: false,
                }];
            }

            line_buffer.clear();
            line_buffer.push_str(&line.text);
            line_buffer.push('\n');

            let regions = match line.kind {
                LineKind::Removed => {
                    old_side.highlight_line(&line_buffer, &assets.syntaxes)
                }
                LineKind::Added => {
                    new_side.highlight_line(&line_buffer, &assets.syntaxes)
                }
                LineKind::Context => {
                    let _ =
                        old_side.highlight_line(&line_buffer, &assets.syntaxes);
                    new_side.highlight_line(&line_buffer, &assets.syntaxes)
                }
                LineKind::Hunk => unreachable!(),
            };

            let Ok(regions) = regions else {
                return vec![Segment {
                    range: 0..line.text.len(),
                    color: (fallback.r, fallback.g, fallback.b),
                    is_emphasis: false,
                }];
            };

            let changed = emphasis[index].as_deref().unwrap_or_default();
            let mut segments =
                Vec::with_capacity(regions.len() + changed.len() * 2);
            let mut cursor = 0;
            let mut emphasis_index = 0;

            for (style, text) in regions {
                let len = text.strip_suffix('\n').unwrap_or(text).len();
                if len == 0 {
                    continue;
                }

                let color = (
                    style.foreground.r,
                    style.foreground.g,
                    style.foreground.b,
                );
                split_region(
                    &mut segments,
                    cursor..cursor + len,
                    color,
                    changed,
                    &mut emphasis_index,
                );
                cursor += len;
            }

            segments
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn byte_ranges_stay_on_utf8_boundaries() {
        let (old, new) = emphasis_ranges("let café = 1", "let tea = 1");
        for range in old {
            assert!("let café = 1".get(range).is_some());
        }
        for range in new {
            assert!("let tea = 1".get(range).is_some());
        }
    }

    /// A space matches across almost any pair of lines. Dropping it from the
    /// run leaves a hole at every word boundary, which reads as a checkerboard
    /// rather than as a marked phrase.
    #[test]
    fn a_common_space_does_not_break_a_marked_phrase() {
        let (old, new) =
            emphasis_ranges("alpha beta gamma", "delta epsilon gamma");

        assert_eq!(old, vec![0..10], "alpha beta");
        assert_eq!(new, vec![0..13], "delta epsilon");
    }

    #[test]
    fn a_rewritten_line_is_tinted_whole_rather_than_token_marked() {
        let lines = vec![
            DiffLine {
                kind: LineKind::Removed,
                text: "# Registration stub: GitHub only registers workflows"
                    .into(),
                old_line: Some(1),
                new_line: None,
            },
            DiffLine {
                kind: LineKind::Added,
                text: "# Creates a presenter copy of a demo baseline".into(),
                old_line: None,
                new_line: Some(1),
            },
        ];

        let emphasis = build_emphasis(&lines);
        assert!(emphasis[0].is_none(), "{:?}", emphasis[0]);
        assert!(emphasis[1].is_none(), "{:?}", emphasis[1]);
    }

    /// The point of the word diff: an edit inside a line still gets marked.
    #[test]
    fn an_edited_argument_is_still_picked_out_of_its_line() {
        let lines = vec![
            DiffLine {
                kind: LineKind::Removed,
                text: "    let total = compute(alpha, options);".into(),
                old_line: Some(1),
                new_line: None,
            },
            DiffLine {
                kind: LineKind::Added,
                text: "    let total = compute(beta, options);".into(),
                old_line: None,
                new_line: Some(1),
            },
        ];

        let emphasis = build_emphasis(&lines);
        let marked = |index: usize, text: &str| {
            emphasis[index]
                .as_ref()
                .expect("an edit keeps its marks")
                .iter()
                .map(|range| text[range.clone()].to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(marked(0, &lines[0].text), vec!["alpha"]);
        assert_eq!(marked(1, &lines[1].text), vec!["beta"]);
    }

    #[test]
    fn generated_lines_take_the_bounded_fallback_path() {
        let text = "x".repeat(MAX_HIGHLIGHT_BYTES + 1);
        let lines = vec![DiffLine {
            kind: LineKind::Context,
            text,
            old_line: Some(1),
            new_line: Some(1),
        }];

        let highlighted =
            highlight_file("generated.js", &lines, ThemeMode::Dark);
        assert_eq!(highlighted[0].len(), 1);
        assert_eq!(highlighted[0][0].range, 0..MAX_HIGHLIGHT_BYTES + 1);
    }

    #[test]
    fn github_defaults_are_used_for_both_modes() {
        assert_eq!(
            assets().dark.settings.foreground,
            Some(syntax_color(0xe6edf3))
        );
        assert_eq!(
            assets().light.settings.foreground,
            Some(syntax_color(0x1f2328))
        );
        assert_eq!(assets().dark.name.as_deref(), Some("GitHub Dark Default"));
        assert_eq!(
            assets().light.name.as_deref(),
            Some("GitHub Light Default")
        );
    }
}
