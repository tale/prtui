use crate::model::{DiffLine, LineKind};
use similar::{ChangeTag, TextDiff};
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::SyntaxSet;

pub struct Segment {
    pub text: String,
    pub color: (u8, u8, u8),
    pub is_emphasis: bool,
}

struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

static ASSETS: OnceLock<Assets> = OnceLock::new();

fn assets() -> &'static Assets {
    ASSETS.get_or_init(|| Assets {
        syntaxes: two_face::syntax::extra_newlines(),
        theme: two_face::theme::extra()
            .get(two_face::theme::EmbeddedThemeName::Nord)
            .clone(),
    })
}

/// Deserializing 213 syntax definitions costs real time; run it on a worker so
/// it overlaps the network fetch instead of stalling the first paint.
pub fn preload() {
    let _ = assets();
}

/// Splits on identifier boundaries rather than whitespace, so `compute(alpha,`
/// becomes `compute` `(` `alpha` `,` and only the argument gets marked.
fn tokenize(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut rest = text;

    while !rest.is_empty() {
        let is_word = |c: char| c.is_alphanumeric() || c == '_';
        let first = rest.chars().next().unwrap();

        let take = if is_word(first) {
            rest.find(|c: char| !is_word(c)).unwrap_or(rest.len())
        } else if first.is_whitespace() {
            rest.find(|c: char| !c.is_whitespace()).unwrap_or(rest.len())
        } else {
            first.len_utf8()
        };

        let (token, tail) = rest.split_at(take);
        tokens.push(token);
        rest = tail;
    }

    tokens
}

/// Per-character mask of the words that actually changed between a removed
/// line and its paired added line.
fn emphasis_masks(old: &str, new: &str) -> (Vec<bool>, Vec<bool>) {
    let old_tokens = tokenize(old);
    let new_tokens = tokenize(new);
    let diff = TextDiff::from_slices(&old_tokens, &new_tokens);

    let mut old_mask = Vec::with_capacity(old.chars().count());
    let mut new_mask = Vec::with_capacity(new.chars().count());

    for change in diff.iter_all_changes() {
        let count = change.value().chars().count();
        match change.tag() {
            ChangeTag::Equal => {
                old_mask.extend(std::iter::repeat_n(false, count));
                new_mask.extend(std::iter::repeat_n(false, count));
            }
            ChangeTag::Delete => old_mask.extend(std::iter::repeat_n(true, count)),
            ChangeTag::Insert => new_mask.extend(std::iter::repeat_n(true, count)),
        }
    }

    (old_mask, new_mask)
}

/// Pairs each run of removed lines with the added run that follows it, so a
/// rewritten line can be shown word-by-word rather than wholly red/green.
fn build_masks(lines: &[DiffLine]) -> Vec<Option<Vec<bool>>> {
    let mut masks: Vec<Option<Vec<bool>>> = vec![None; lines.len()];

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

        // Only a balanced rewrite maps cleanly; anything else is a pure
        // insertion or deletion and reads better fully tinted.
        if removed.len() != added.len() {
            continue;
        }

        for (old_index, new_index) in removed.zip(added) {
            let (old_mask, new_mask) =
                emphasis_masks(&lines[old_index].text, &lines[new_index].text);

            masks[old_index] = Some(old_mask);
            masks[new_index] = Some(new_mask);
        }
    }

    masks
}

fn split_by_mask(text: &str, color: (u8, u8, u8), mask: &[bool], cursor: usize) -> Vec<Segment> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }

    let mut segments = Vec::new();
    let mut run_start = 0;

    while run_start < chars.len() {
        let flag = mask.get(cursor + run_start).copied().unwrap_or(false);

        let mut run_end = run_start;
        while run_end < chars.len()
            && mask.get(cursor + run_end).copied().unwrap_or(false) == flag
        {
            run_end += 1;
        }

        segments.push(Segment {
            text: chars[run_start..run_end].iter().collect(),
            color,
            is_emphasis: flag,
        });

        run_start = run_end;
    }

    segments
}

pub fn highlight_file(path: &str, lines: &[DiffLine]) -> Vec<Vec<Segment>> {
    let assets = assets();

    let syntax = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .and_then(|ext| assets.syntaxes.find_syntax_by_extension(ext))
        .unwrap_or_else(|| assets.syntaxes.find_syntax_plain_text());

    // Each side keeps its own parser state so a hunk that only touches one
    // side does not corrupt the other's string/comment nesting.
    let mut old_side = HighlightLines::new(syntax, &assets.theme);
    let mut new_side = HighlightLines::new(syntax, &assets.theme);

    let masks = build_masks(lines);

    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            if line.kind == LineKind::Hunk {
                return Vec::new();
            }

            let owned = format!("{}\n", line.text);

            let regions = match line.kind {
                LineKind::Removed => old_side.highlight_line(&owned, &assets.syntaxes),
                LineKind::Added => new_side.highlight_line(&owned, &assets.syntaxes),
                _ => {
                    let _ = old_side.highlight_line(&owned, &assets.syntaxes);
                    new_side.highlight_line(&owned, &assets.syntaxes)
                }
            };

            let Ok(regions) = regions else {
                return vec![Segment {
                    text: line.text.clone(),
                    color: (216, 222, 233),
                    is_emphasis: false,
                }];
            };

            let mask = masks[index].as_deref().unwrap_or(&[]);

            let mut segments = Vec::new();
            let mut cursor = 0;
            for (style, text) in regions {
                let text = text.strip_suffix('\n').unwrap_or(text);
                if text.is_empty() {
                    continue;
                }

                let color = (style.foreground.r, style.foreground.g, style.foreground.b);
                segments.extend(split_by_mask(text, color, mask, cursor));
                cursor += text.chars().count();
            }

            segments
        })
        .collect()
}
