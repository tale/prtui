//! Drawing.
//!
//! Every function here reads app state and the frame's [`Layout`] and writes
//! only to the frame. Where things go and how tall they are was decided by the
//! layout, so nothing has to be discovered mid-render and written back.

mod tree;

use crate::app::draft::{Side, Sync};
use crate::app::keymap::Reference;
use crate::app::mode::Mode;
use crate::app::review::ReviewEvent;
use crate::app::search::Query;
use crate::app::{App, Focus, OpenFile, Pane, Target};
use crate::expand::Gap;
use crate::layout::rows::{self, BodyRow, GUTTER, Row, ThreadState};
use crate::layout::{Content, Layout};
use crate::model::{LineKind, ReviewThread};
use crate::renderer::{Theme, markdown};
use crate::text::measure::{clip_text_to_budget, text_width, truncate};
use crate::text::wrap::{self, Fragment};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// Frames of the wait indicator, shared with the pull request selector.
pub const SPINNER: [&str; 10] =
    ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

#[derive(Clone, Copy)]
pub enum ExitHint {
    Quit,
    Back,
}

impl ExitHint {
    const fn label(self) -> &'static str {
        match self {
            Self::Quit => "quit",
            Self::Back => "back",
        }
    }
}

pub fn draw(
    frame: &mut Frame,
    app: &App,
    layout: &Layout,
    exit_hint: ExitHint,
) {
    draw_header(frame, app, layout.header);

    if app.is_loading() {
        draw_loading(frame, app, layout.body);
        draw_bottom_bar(frame, app, layout, exit_hint);
        return;
    }

    tree::draw(frame, app, layout);
    draw_diff(frame, app, layout);

    draw_bottom_bar(frame, app, layout, exit_hint);
    draw_composer(frame, app, layout);
    draw_submit(frame, app, layout);
    draw_overlay(frame, app, layout);
}

/// Columns the reference spends on its indent, on the chord, and on the command
/// name, leaving the rest of the line for what the command does.
const HELP_INDENT: usize = 2;
const HELP_KEYS: usize = 18;
const HELP_NAME: usize = 20;

// The reference is styled here rather than in the layout because its columns
// are budgeted against the width it is painted at.
fn draw_overlay(frame: &mut Frame, app: &App, layout: &Layout) {
    let Some(overlay) = layout.overlay.as_ref() else {
        return;
    };
    let theme = app.theme();

    let block = docked_block(overlay.title.to_owned(), theme.accent)
        .title_bottom(
            Line::styled(
                " j/k scroll · / find · esc close ",
                Style::default().fg(theme.dim),
            )
            .right_aligned(),
        );
    frame.render_widget(Clear, overlay.area);
    frame.render_widget(block, overlay.area);

    let width = overlay.inner.width as usize;
    let scroll = app.overlay_scroll;
    let height = overlay.inner.height as usize;
    let query = app.live_query();
    let current = app.overlay_match_row(layout);
    let hit = |row: usize| {
        Style::default().bg(if Some(row) == current {
            theme.search_current
        } else {
            theme.search
        })
    };

    let lines: Vec<Line> = match &overlay.content {
        Content::Keys(entries) => entries
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(row, entry)| {
                paint_hits(help_line(entry, width, theme), query, hit(row))
            })
            .collect(),
        Content::Prose(prose) => prose
            .iter()
            .enumerate()
            .skip(scroll)
            .take(height)
            .map(|(row, line)| paint_hits(line.clone(), query, hit(row)))
            .collect(),
    };

    frame.render_widget(Paragraph::new(lines), overlay.inner);
}

// Hits are found per span, so a match straddling a style change is stepped to
// but left unpainted.
fn paint_hits(
    line: Line<'static>,
    query: Option<Query<'_>>,
    hit: Style,
) -> Line<'static> {
    let Some(query) = query else {
        return line;
    };

    let spans: Vec<Span<'static>> = line
        .spans
        .iter()
        .flat_map(|span| {
            matched_spans(
                span.content.to_string(),
                Some(query),
                span.style,
                span.style.patch(hit),
            )
        })
        .collect();

    Line::from(spans).style(line.style)
}

fn help_line(line: &Reference, width: usize, theme: Theme) -> Line<'static> {
    match line {
        Reference::Heading(title) => Line::styled(
            (*title).to_owned(),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Reference::Entry {
            keys,
            name,
            summary,
        } => {
            let budget =
                width.saturating_sub(HELP_INDENT + HELP_KEYS + HELP_NAME);

            Line::from(vec![
                Span::styled(
                    format!(
                        "{:HELP_INDENT$}{:HELP_KEYS$}",
                        "",
                        truncate(keys, HELP_KEYS)
                    ),
                    Style::default().fg(theme.heading),
                ),
                Span::styled(
                    format!("{:HELP_NAME$}", truncate(name, HELP_NAME)),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(
                    truncate(summary, budget),
                    Style::default().fg(theme.dim),
                ),
            ])
        }
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let spans = match &app.pr {
        None => vec![Span::styled(
            " prtui ",
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        )],
        Some(pr) => {
            let (label, color) = match pr.state.as_str() {
                "MERGED" => (" merged ", theme.purple),
                "CLOSED" => (" closed ", theme.danger),
                _ if pr.is_draft => (" draft ", theme.dim),
                _ => (" open ", theme.success),
            };

            // Each part takes what is left after the ones before it, so the row
            // ends on an ellipsis rather than in the middle of a word. The
            // title outranks the branches, which outrank the author.
            let mut budget = area.width as usize;
            let mut take = |text: String, style: Style| {
                let clipped = truncate(&text, budget);
                budget = budget.saturating_sub(text_width(&clipped));

                Span::styled(clipped, style)
            };

            vec![
                take(
                    label.to_string(),
                    Style::default()
                        .bg(color)
                        .fg(theme.ink)
                        .add_modifier(Modifier::BOLD),
                ),
                take(
                    format!(" #{} ", pr.number),
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                take(
                    pr.title.clone(),
                    Style::default()
                        .fg(theme.heading)
                        .add_modifier(Modifier::BOLD),
                ),
                take(
                    format!("  {} → {}", pr.head_ref, pr.base_ref),
                    Style::default().fg(theme.muted),
                ),
                take(
                    format!("  @{}", pr.author),
                    Style::default().fg(theme.dim),
                ),
            ]
        }
    };

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn pane_block<'a>(title: String, is_focused: bool, theme: Theme) -> Block<'a> {
    Block::default()
        .border_style(Style::default().fg(if is_focused {
            theme.accent
        } else {
            theme.dim
        }))
        .title(Span::styled(
            title,
            Style::default()
                .fg(if is_focused {
                    theme.heading
                } else {
                    theme.muted
                })
                .add_modifier(if is_focused {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ))
}

/// Splits one run of text around the filter's hits, so a match reads in the
/// path itself rather than being implied by the file still being listed.
///
/// The query ran against the whole path, but the tree elides what will not fit;
/// matching the visible text again is what keeps the paint on the right bytes.
fn matched_spans(
    text: String,
    query: Option<Query<'_>>,
    base: Style,
    hit: Style,
) -> Vec<Span<'static>> {
    let hits = query.map_or_else(Vec::new, |query| query.ranges(&text));
    if hits.is_empty() {
        return vec![Span::styled(text, base)];
    }

    let mut spans = Vec::with_capacity(hits.len() * 2 + 1);
    let mut at = 0;

    for range in hits {
        if at < range.start {
            spans.push(Span::styled(text[at..range.start].to_string(), base));
        }
        spans.push(Span::styled(text[range.clone()].to_string(), hit));
        at = range.end;
    }

    if at < text.len() {
        spans.push(Span::styled(text[at..].to_string(), base));
    }

    spans
}

fn draw_diff(frame: &mut Frame, app: &App, layout: &Layout) {
    let theme = app.theme();
    let is_focused = app.pane == Pane::Diff;
    let area = layout.diff;
    let title = app.current_file().map_or_else(
        || " Diff ".to_string(),
        |file| {
            let comments =
                app.threads_by_path.get(&file.path).map_or(0, |threads| {
                    threads.iter().filter(|thread| !thread.is_resolved).count()
                });
            let suffix = if comments == 0 {
                format!("  +{} -{}", file.additions, file.deletions)
            } else {
                format!(
                    "  ◆ {comments}  +{} -{}",
                    file.additions, file.deletions
                )
            };
            let available = layout.diff_pane.width.saturating_sub(4) as usize;
            format!(
                " {}{} ",
                truncate(
                    &file.path,
                    available.saturating_sub(text_width(&suffix))
                ),
                suffix
            )
        },
    );

    frame.render_widget(
        pane_block(title, is_focused, theme).borders(Borders::TOP),
        layout.diff_pane,
    );

    let Some(open) = app.open() else {
        draw_centered(frame, area, "no diff selected", theme.dim);
        return;
    };

    let width = area.width as usize;
    let focus = app.focus();
    let lines: Vec<Line<'_>> = layout
        .rows
        .window(app.diff_scroll, area.height as usize)
        .iter()
        .map(|row| match row {
            Row::Code { source, fragment } => {
                code_line(&open, focus, *source, *fragment, width, theme)
            }
            Row::Gap { gap } => gap_line(layout.gaps.get(*gap), width, theme),
            Row::Draft { draft } => {
                draft_line(&open, focus, *draft, width, theme)
            }
            Row::Divider => divider_line(width, theme),
            Row::Summary {
                thread,
                state,
                author_width,
            } => summary_line(
                focus,
                &open.threads[*thread],
                *state,
                *author_width,
                width,
                theme,
            ),
            Row::Body { index } => {
                body_row(layout.rows.body(*index), width, theme)
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
}

/// The band standing in for a run of the file the patch left out.
///
/// It carries the hunk band's colours because it is the same kind of thing: a
/// break in the file rather than a line of it. What it says is how much is
/// missing, which is what the reader decides on before opening it.
fn gap_line(gap: Option<&Gap>, width: usize, theme: Theme) -> Line<'static> {
    let hidden = match gap.map(|gap| gap.len) {
        Some(Some(len)) => format!("{len} lines hidden"),
        // The last run ends where the file does, which only the file says.
        _ => "rest of the file".to_string(),
    };
    let text = format!(" ⋯  {hidden:<width$}", width = width.saturating_sub(4));

    Line::from(Span::styled(
        text,
        Style::default()
            .bg(theme.hunk)
            .fg(theme.muted)
            .add_modifier(Modifier::ITALIC),
    ))
}

fn code_line<'a>(
    open: &OpenFile<'a>,
    focus: Focus<'_>,
    index: usize,
    fragment: Fragment,
    width: usize,
    theme: Theme,
) -> Line<'a> {
    let Some(line) = open.line(index) else {
        return Line::default();
    };

    let is_cursor = focus.is_cursor(index);
    let is_selected = focus.is_selected(index);

    if line.kind == LineKind::Hunk {
        let text = format!("{:<width$}", line.text, width = width);
        let bg = if is_selected {
            theme.selection
        } else {
            theme.hunk
        };

        return Line::from(Span::styled(
            text,
            Style::default()
                .bg(bg)
                .fg(theme.muted)
                .add_modifier(Modifier::ITALIC),
        ));
    }

    let (base_bg, strong_bg, sigil) = match line.kind {
        LineKind::Added => (theme.add, theme.add_emphasis, "+"),
        LineKind::Removed => (theme.delete, theme.delete_emphasis, "-"),
        _ => (theme.background, theme.background, " "),
    };

    // Selected rows keep their add/remove identity and are shifted instead of
    // flattened; the left bar is what makes the span read as contiguous.
    let bg = match (is_selected, is_cursor) {
        (true, _) => theme.selection_background(base_bg),
        (false, true) => theme.cursor_background(base_bg),
        _ => base_bg,
    };

    let has_thread = open.threads.iter().any(|thread| {
        !thread.is_outdated && thread.anchors_to(line) && !thread.is_resolved
    });
    let draft = open
        .drafts
        .iter()
        .find(|draft| draft.rows().is_some_and(|rows| rows.contains(&index)));

    // A draft GitHub has not accepted yet says so in the gutter: the review is
    // held on the server now, so the two can be out of step.
    let (marker, marker_color) = match (draft, has_thread) {
        (Some(draft), _) => {
            (draft.sync.marker(), draft_tint(&draft.sync, theme))
        }
        (None, true) => (" ◆", theme.purple),
        _ => ("  ", theme.dim),
    };

    // The bar runs down every row of a folded line so the whole of it reads as
    // one block, but the numbers, marker and sigil belong to its first row only.
    let mut spans = vec![];

    if fragment.is_first {
        spans.push(Span::styled(
            format!(
                "{:>4} {:>4}",
                line.old_line.map(|n| n.to_string()).unwrap_or_default(),
                line.new_line.map(|n| n.to_string()).unwrap_or_default(),
            ),
            Style::default().bg(bg).fg(theme.dim),
        ));
        spans.push(Span::styled(
            marker,
            Style::default().bg(bg).fg(marker_color),
        ));
        spans.push(Span::styled(sigil, Style::default().bg(bg).fg(theme.dim)));
    } else {
        spans.push(Span::styled(" ".repeat(GUTTER), Style::default().bg(bg)));
    }

    let colored: Vec<Piece> =
        match open.segments(index).filter(|segments| !segments.is_empty()) {
            Some(segments) => segments
                .iter()
                .map(|segment| Piece {
                    range: segment.range.clone(),
                    color: Color::Rgb(
                        segment.color.0,
                        segment.color.1,
                        segment.color.2,
                    ),
                    is_emphasis: segment.is_emphasis,
                    is_match: false,
                })
                .collect(),
            None => vec![Piece {
                range: 0..line.text.len(),
                color: theme.code,
                is_emphasis: false,
                is_match: false,
            }],
        };

    let mut used = GUTTER;
    for piece in split_by_matches(colored, &focus.matches(&line.text)) {
        // Syntax runs and search hits span the whole line; this row shows one
        // slice of it, so each run is trimmed to what falls inside the fragment.
        let start = piece.range.start.max(fragment.start);
        let end = piece.range.end.min(fragment.end);
        if start >= end {
            continue;
        }

        let Some(source) = line.text.get(start..end) else {
            continue;
        };
        let (text, display_width) = clip_text_to_budget(
            source,
            width.saturating_sub(used),
            fragment.column + used - GUTTER,
        );
        if text.is_empty() {
            break;
        }
        used += display_width;

        let is_plain = !is_cursor && !is_selected;
        let seg_bg = match (piece.is_match, piece.is_emphasis && is_plain) {
            (true, _) if is_cursor => theme.search_current,
            (true, _) => theme.search,
            (false, true) => strong_bg,
            (false, false) => bg,
        };
        spans.push(Span::styled(
            text,
            Style::default().bg(seg_bg).fg(piece.color),
        ));
    }

    spans.push(Span::styled(
        " ".repeat(width.saturating_sub(used)),
        Style::default().bg(bg),
    ));

    Line::from(spans)
}

/// One run of diff text that shares a foreground color and a background role.
struct Piece {
    range: std::ops::Range<usize>,
    color: Color,
    is_emphasis: bool,
    is_match: bool,
}

impl Piece {
    const fn slice(
        &self,
        range: std::ops::Range<usize>,
        is_match: bool,
    ) -> Self {
        Self {
            range,
            color: self.color,
            is_emphasis: self.is_emphasis,
            is_match,
        }
    }
}

/// Cuts syntax runs at search-hit boundaries so a match repaints exactly the
/// bytes it covers. `matches` must be sorted and non-overlapping, which is what
/// the search produces.
fn split_by_matches(
    colored: Vec<Piece>,
    matches: &[std::ops::Range<usize>],
) -> Vec<Piece> {
    if matches.is_empty() {
        return colored;
    }

    let mut pieces = Vec::with_capacity(colored.len());
    for piece in colored {
        let mut at = piece.range.start;

        for hit in matches {
            let start = hit.start.max(piece.range.start);
            let end = hit.end.min(piece.range.end);
            if start >= end {
                continue;
            }

            if at < start {
                pieces.push(piece.slice(at..start, false));
            }
            pieces.push(piece.slice(start..end, true));
            at = end;
        }

        if at < piece.range.end {
            pieces.push(piece.slice(at..piece.range.end, false));
        }
    }

    pieces
}

const fn state_color(state: ThreadState, theme: Theme) -> Color {
    match state {
        ThreadState::Open => theme.purple,
        ThreadState::Resolved => theme.success,
        ThreadState::Outdated => theme.warning,
    }
}

fn card_indent(width: usize) -> usize {
    GUTTER.min(width.saturating_sub(1))
}

/// How far a draft has got towards the copy GitHub holds: waiting, saved, or
/// refused. The gutter and the card have to agree on it.
const fn draft_tint(sync: &Sync, theme: Theme) -> Color {
    match sync {
        Sync::Failed(_) => theme.danger,
        Sync::Synced => theme.orange,
        _ => theme.dim,
    }
}

/// A pending remark of the reader's own, under the line it answers to — or, for
/// one about the whole file, leading the pane the way GitHub draws them. A
/// review can then be read back before it ships rather than one draft at a time
/// through the editor. A refusal names its reason here: the gutter has room to
/// say only that something went wrong.
///
/// It focuses and expands like any thread card, since it is a comment the same
/// as any other. Expanded, it gives its one line up to the body underneath.
fn draft_line(
    open: &OpenFile<'_>,
    focus: Focus<'_>,
    index: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let Some(draft) = open.drafts.get(index) else {
        return Line::default();
    };

    let indent = card_indent(width);
    let card_width = width.saturating_sub(indent);
    let is_focused = focus.is_draft_focused(draft.id);
    let is_expanded = focus.is_draft_expanded(draft.id);

    let label = if draft.is_file_level() {
        "file note"
    } else {
        "draft"
    };
    let suffix = match &draft.sync {
        Sync::Synced => String::new(),
        Sync::Failed(error) => format!(" · {error}"),
        _ => " · saving".to_string(),
    };
    let separator = if is_expanded { "" } else { "  " };
    let summary = if is_expanded {
        String::new()
    } else {
        comment_summary(&draft.body, theme)
    };

    let tint = draft_tint(&draft.sync, theme);
    let marker = draft.sync.marker().trim();
    let prefix = if is_expanded {
        format!("{marker} ▾ ")
    } else {
        format!("{marker} ")
    };

    let fixed = text_width(&prefix)
        + text_width(label)
        + text_width(separator)
        + text_width(&suffix);
    let summary = truncate(&summary, card_width.saturating_sub(fixed));

    thread_card_line(
        indent,
        width,
        vec![
            rows::card_span(prefix, tint, Modifier::BOLD, theme),
            rows::card_span(label, theme.heading, Modifier::BOLD, theme),
            rows::card_span(separator, theme.muted, Modifier::empty(), theme),
            rows::card_span(summary, theme.code, Modifier::empty(), theme),
            rows::card_span(suffix, tint, Modifier::empty(), theme),
        ],
        theme,
        is_focused,
    )
}

/// The seam between two cards stacked under one line. It spans the card rather
/// than the pane: what it divides is the block of conversations, not the diff.
fn divider_line(width: usize, theme: Theme) -> Line<'static> {
    let indent = card_indent(width);

    thread_card_line(
        indent,
        width,
        vec![rows::card_span(
            "─".repeat(width.saturating_sub(indent)),
            theme.muted,
            Modifier::empty(),
            theme,
        )],
        theme,
        false,
    )
}

fn summary_line(
    focus: Focus<'_>,
    thread: &ReviewThread,
    state: ThreadState,
    author_width: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let indent = card_indent(width);
    let card_width = width.saturating_sub(indent);
    let is_focused = focus.is_thread_focused(&thread.id);
    let is_expanded = focus.is_thread_expanded(&thread.id);

    // Settled threads are history beside the open ones stacked with them, so
    // they read back rather than competing for the eye. The marker keeps its
    // colour: which kind of settled still has to be legible.
    let (author_color, summary_color) = if state.is_settled() {
        (theme.muted, theme.muted)
    } else {
        (theme.heading, theme.code)
    };

    // An expanded card gives its one line to the conversation's shape instead
    // of repeating the first comment, which is about to be printed underneath.
    let prefix = if is_expanded {
        format!("{} ▾ ", state.marker())
    } else {
        format!("{} ", state.marker())
    };
    let author = if is_expanded {
        String::new()
    } else {
        truncate(&rows::author_of(thread), author_width)
    };
    // The author column and the gap after it, held even by a short name so the
    // summaries down a stack start together.
    let author_column = if is_expanded { 0 } else { author_width + 2 };
    let pad = author_column.saturating_sub(text_width(&author));

    let summary = if is_expanded {
        comment_count(thread.comments.len())
    } else {
        thread.comments.first().map_or_else(
            || "no comment body".into(),
            |comment| comment_summary(&comment.body, theme),
        )
    };

    let tail = summary_tail(thread, state, is_expanded);
    let head = text_width(&prefix) + text_width(&author) + pad;
    let reserved = if tail.is_empty() {
        0
    } else {
        text_width(&tail) + 2
    };

    let summary =
        truncate(&summary, card_width.saturating_sub(head + reserved));
    let filler = card_width
        .saturating_sub(head + text_width(&summary) + text_width(&tail));

    thread_card_line(
        indent,
        width,
        vec![
            rows::card_span(
                prefix,
                state_color(state, theme),
                Modifier::BOLD,
                theme,
            ),
            rows::card_span(author, author_color, Modifier::BOLD, theme),
            rows::card_span(
                " ".repeat(pad),
                theme.muted,
                Modifier::empty(),
                theme,
            ),
            rows::card_span(summary, summary_color, Modifier::empty(), theme),
            rows::card_span(
                " ".repeat(filler),
                theme.muted,
                Modifier::empty(),
                theme,
            ),
            rows::card_span(tail, theme.muted, Modifier::empty(), theme),
        ],
        theme,
        is_focused,
    )
}

/// What a summary carries at the card's right edge: how much conversation is
/// folded away, and whether the thread is still live. Sitting flush right keeps
/// it in one column down a stack of threads rather than trailing each summary.
fn summary_tail(
    thread: &ReviewThread,
    state: ThreadState,
    is_expanded: bool,
) -> String {
    let replies = thread.comments.len().saturating_sub(1);
    let mut tail = match (is_expanded, replies) {
        (true, _) | (_, 0) => String::new(),
        (false, 1) => "1 reply".to_string(),
        (false, count) => format!("{count} replies"),
    };

    if state.is_settled() {
        if !tail.is_empty() {
            tail.push_str("  ");
        }
        tail.push_str(state.label());
    }

    tail
}

fn body_row(
    body: Option<&BodyRow>,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let Some(body) = body else {
        return Line::default();
    };

    let (rail, color) = if body.is_thumb {
        ("  ┃ ", theme.purple)
    } else {
        ("  │ ", theme.dim)
    };

    let mut spans = body.spans.clone();
    spans.insert(0, rows::card_span(rail, color, Modifier::empty(), theme));

    thread_card_line(card_indent(width), width, spans, theme, false)
}

fn comment_count(count: usize) -> String {
    match count {
        1 => "1 comment".into(),
        count => format!("{count} comments"),
    }
}

fn comment_summary(body: &str, theme: Theme) -> String {
    markdown::render(body, 4096, theme)
        .into_iter()
        .map(|line| {
            line.spans
                .into_iter()
                .map(|span| span.content.into_owned())
                .collect::<String>()
        })
        .find(|line| !line.trim().is_empty())
        .unwrap_or_else(|| "no comment body".into())
}

fn thread_card_line(
    indent: usize,
    width: usize,
    mut spans: Vec<Span<'static>>,
    theme: Theme,
    is_focused: bool,
) -> Line<'static> {
    let background = if is_focused { theme.cursor } else { theme.hunk };
    for span in &mut spans {
        if is_focused || span.style.bg.is_none() {
            span.style = span.style.bg(background);
        }
    }

    let used = spans.iter().map(Span::width).sum::<usize>();
    let card_width = width.saturating_sub(indent);
    spans.insert(0, Span::raw(" ".repeat(indent)));
    spans.push(Span::styled(
        " ".repeat(card_width.saturating_sub(used)),
        Style::default().bg(background),
    ));

    Line::from(spans)
}

/// Sits below the diff rather than over it, so the lines being commented on stay
/// on screen while typing.
fn draw_composer(frame: &mut Frame, app: &App, layout: &Layout) {
    let theme = app.theme();
    let (Some(composer), Some(rect)) = (app.composer.as_ref(), layout.composer)
    else {
        return;
    };

    let name = composer.path.rsplit('/').next().unwrap_or(&composer.path);
    let title = match &composer.target {
        Target::Reply { .. } => format!(" reply · {name} "),
        Target::File { replacing } => {
            let verb = if replacing.is_some() {
                "edit file note"
            } else {
                "file note"
            };
            format!(" {verb} · {name} ")
        }
        Target::Line {
            anchor, replacing, ..
        } => {
            let verb = if replacing.is_some() {
                "edit draft"
            } else {
                "comment"
            };
            // A span that crosses sides counts in two files at once, so both
            // ends have to name the one they belong to.
            if anchor.start_side == anchor.side {
                let span = if anchor.start_line == anchor.end_line {
                    format!("{}", anchor.end_line)
                } else {
                    format!("{}-{}", anchor.start_line, anchor.end_line)
                };

                // Commenting on the new file is the ordinary thing to be
                // doing, so only the old side is worth naming.
                match anchor.side {
                    Side::Left => format!(" {verb} · {name}:{span} · old "),
                    Side::Right => format!(" {verb} · {name}:{span} "),
                }
            } else {
                format!(
                    " {verb} · {name}:{} {} → {} {} ",
                    anchor.start_line,
                    anchor.start_side.label(),
                    anchor.end_line,
                    anchor.side.label()
                )
            }
        }
    };

    let (row, column) = composer.editor.cursor();
    let position = format!(
        " ln {} · col {} ",
        row + 1,
        composer.editor.lines()[row][..column].chars().count() + 1
    );
    let (footer, footer_color) = if composer.is_discard_armed {
        (" esc again to discard ".to_string(), theme.danger)
    } else {
        (position, theme.dim)
    };

    let block = docked_block(title, theme.orange).title_bottom(
        Line::styled(footer, Style::default().fg(footer_color)).right_aligned(),
    );
    let inner = block.inner(rect);

    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    if inner.is_empty() {
        return;
    }

    draw_editor_body(frame, &composer.editor, inner, theme, None);
}

fn docked_block<'a>(title: String, color: Color) -> Block<'a> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(Span::styled(
            title,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ))
}

/// Draws a multiline editor with its long lines folded rather than scrolled
/// sideways, and parks the terminal cursor where the next character will land.
///
/// The row being typed on is painted across the full width as well. A blinking
/// cell is easy to lose against a wall of text, and a docked editor sits under
/// a diff that carries a cursor bar of its own.
fn draw_editor_body(
    frame: &mut Frame,
    editor: &crate::app::editor::CommentEditor,
    area: Rect,
    theme: Theme,
    placeholder: Option<&str>,
) {
    let lines = editor.lines();

    if let Some(hint) =
        placeholder.filter(|_| lines.iter().all(String::is_empty))
    {
        frame.render_widget(
            Paragraph::new(Line::styled(
                hint.to_string(),
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
            )),
            area,
        );
        frame.set_cursor_position((area.x, area.y));
        return;
    }

    let width = area.width as usize;
    let wrapped = wrap::Wrapped::new(lines, width);
    let (line, byte) = editor.cursor();
    let (cursor_row, cursor_column) = wrapped.locate(line, byte);

    // Folding means the cursor can only ever leave the viewport vertically.
    let height = area.height as usize;
    let first_row = editor_scroll(cursor_row, wrapped.rows().len(), height);
    let visible: Vec<Line> = wrapped
        .rows()
        .iter()
        .enumerate()
        .skip(first_row)
        .take(height)
        .map(|(index, row)| {
            let text = wrapped.text(*row);
            let style = if index == cursor_row {
                Style::default().fg(theme.code).bg(theme.cursor)
            } else {
                Style::default().fg(theme.code)
            };
            let padding = width.saturating_sub(text_width(text));

            Line::from(vec![
                Span::styled(text.to_string(), style),
                Span::styled(" ".repeat(padding), style),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(visible), area);
    frame.set_cursor_position((
        area.x + cursor_column.min(width.saturating_sub(1)) as u16,
        area.y + (cursor_row - first_row) as u16,
    ));
}

/// First visual row of an editor whose text has outgrown its box.
///
/// The view holds no scroll of its own, so this is a function of where the
/// cursor is. Keeping it near the middle rather than pinned to the last row
/// leaves the lines below it readable while they are being written.
const fn editor_scroll(cursor: usize, total: usize, height: usize) -> usize {
    if total <= height || height == 0 {
        return 0;
    }

    let centred = cursor.saturating_sub(height / 2);
    if centred > total - height {
        total - height
    } else {
        centred
    }
}

/// The verdict reads as three chips because the review has to be sendable
/// without leaving the summary field to pick one.
fn event_chips(active: ReviewEvent, theme: Theme) -> Vec<Span<'static>> {
    ReviewEvent::ALL
        .iter()
        .map(|event| {
            let color = match event {
                ReviewEvent::Approve => theme.success,
                ReviewEvent::RequestChanges => theme.danger,
                ReviewEvent::Comment => theme.accent,
            };

            if *event == active {
                Span::styled(
                    format!(" {} ", event.label()),
                    Style::default()
                        .bg(color)
                        .fg(theme.ink)
                        .add_modifier(Modifier::BOLD),
                )
            } else {
                Span::styled(
                    format!(" {} ", event.label()),
                    Style::default().fg(theme.dim),
                )
            }
        })
        .collect()
}

/// Rows the last rejection gets before it is cut off. A validation failure runs
/// to a couple of lines; anything longer is a wall of JSON.
const REJECTION_ROWS: usize = 3;

/// Docked where the composer goes, for the same reason: the drafts being shipped
/// stay readable behind the summary being written about them.
fn draw_submit(frame: &mut Frame, app: &App, layout: &Layout) {
    let theme = app.theme();
    let (Some(submission), Some(rect)) =
        (app.submission.as_ref(), layout.submit)
    else {
        return;
    };

    let title = match app.drafts.len() {
        0 => " submit review ".to_string(),
        1 => " submit review · 1 draft ".to_string(),
        count => format!(" submit review · {count} drafts "),
    };

    let block = if submission.is_discard_armed {
        docked_block(title, theme.accent).title_bottom(
            Line::styled(
                " esc again to discard ",
                Style::default().fg(theme.danger),
            )
            .right_aligned(),
        )
    } else {
        docked_block(title, theme.accent)
    };
    let inner = block.inner(rect);
    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    if inner.height < 3 {
        return;
    }

    frame.render_widget(
        Paragraph::new(Line::from(event_chips(submission.event, theme))),
        Rect { height: 1, ..inner },
    );
    frame.render_widget(
        Paragraph::new(Line::styled(
            "─".repeat(inner.width as usize),
            Style::default().fg(theme.dim),
        )),
        Rect {
            y: inner.y + 1,
            height: 1,
            ..inner
        },
    );

    let reason = submission.error.as_deref().unwrap_or_default();
    let rows: Vec<&str> = wrap::fragments(reason, inner.width as usize)
        .into_iter()
        .take(REJECTION_ROWS)
        .filter(|fragment| !fragment.is_empty())
        .map(|fragment| &reason[fragment.start..fragment.end])
        .collect();

    for (offset, text) in rows.iter().enumerate() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                (*text).to_string(),
                Style::default().fg(theme.danger),
            )),
            Rect {
                y: inner.y + 2 + offset as u16,
                height: 1,
                ..inner
            },
        );
    }

    let used = 2 + rows.len() as u16;
    if inner.height <= used {
        return;
    }

    draw_editor_body(
        frame,
        &submission.editor,
        Rect {
            y: inner.y + used,
            height: inner.height - used,
            ..inner
        },
        theme,
        Some("summary (optional for approve)"),
    );
}

/// The status bar's own background, which every span on it is drawn over.
pub fn bar_style(theme: Theme) -> Style {
    Style::default().bg(theme.hunk)
}

/// The mode's name in its own color, which is what opens every status bar.
pub fn mode_chip(mode: Mode, theme: Theme) -> Span<'static> {
    let background = match mode {
        Mode::Normal => theme.accent,
        Mode::Visual => theme.orange,
        Mode::Insert => theme.success,
        Mode::Filter => theme.purple,
        Mode::Search => theme.warning,
        Mode::CommandLine => theme.muted,
        Mode::Help | Mode::Overview | Mode::Submit => theme.heading,
    };

    Span::styled(
        mode.label(),
        Style::default()
            .bg(background)
            .fg(theme.ink)
            .add_modifier(Modifier::BOLD),
    )
}

/// Paints one status bar: what the surface has to say, then as many key hints
/// as the tail has room for.
///
/// `reserved` claims its width before the hints do, so a key that has to stay
/// reachable survives a narrow terminal that drops the rest.
pub fn draw_status_bar(
    frame: &mut Frame,
    area: Rect,
    left: Vec<Span<'static>>,
    hints: &[(&str, &str)],
    reserved: &[(&str, &str)],
    theme: Theme,
) {
    let bar = bar_style(theme);

    frame.render_widget(
        Paragraph::new(Span::styled(" ".repeat(area.width as usize), bar)),
        area,
    );

    let left = Line::from(left);
    let left_width = left.width();
    frame.render_widget(Paragraph::new(left), area);

    draw_hints(frame, area, left_width, hints, reserved, bar, theme);
}

fn draw_bottom_bar(
    frame: &mut Frame,
    app: &App,
    layout: &Layout,
    exit_hint: ExitHint,
) {
    let area = layout.status;
    let pending_hint = app.pending_hint();
    let theme = app.theme();
    let bar = bar_style(theme);

    let pane = match app.pane {
        Pane::Files => " files",
        Pane::Diff => " diff",
    };

    let show_search_position = app.search.is_some()
        && (app.pane == Pane::Diff || app.is_searching_overlay());
    let show_match_position = app.mode == Mode::Filter
        || (app.mode == Mode::Normal
            && app.pane == Pane::Files
            && app.file_filter.is_some());
    let position = match (
        show_search_position,
        show_match_position,
        app.current_file(),
    ) {
        (true, _, _) => {
            let (current, total) = app.search_summary(layout);
            format!("  {current}/{total} matches")
        }
        (false, true, _) => format!(
            "  {}/{} matches",
            layout.files.file_position(app.selected_file),
            layout.files.file_count()
        ),
        // Two bare ratios said nothing about what they counted.
        (false, false, Some(file)) => format!(
            "  file {}/{} · line {}/{}",
            layout.files.file_position(app.selected_file),
            app.files.len(),
            (app.cursor + 1).min(file.lines.len().max(1)),
            file.lines.len()
        ),
        (false, false, None) => String::new(),
    };

    let mut spans = vec![
        mode_chip(app.mode, theme),
        Span::styled(pane, bar.fg(theme.dim)),
    ];

    // The `:` line and the search box are the same widget in the same place,
    // so an open command line covers the query rather than sitting beside it.
    let prompt = app.command_line.as_ref().map_or_else(
        || {
            app.search
                .as_ref()
                .map(|editor| ('/', editor, theme.warning))
        },
        |editor| Some((':', editor, theme.accent)),
    );

    let mut prompt_column = None;
    if let Some((sigil, editor, color)) = prompt {
        let query = editor.lines()[0].clone();
        let (_, cursor_byte) = editor.cursor();

        prompt_column = Some(
            Line::from(spans.clone()).width()
                + 3
                + text_width(&query[..cursor_byte]),
        );
        spans.push(Span::styled(format!("  {sigil}"), bar.fg(color)));
        spans.push(Span::styled(query, bar.fg(theme.heading)));
    }

    spans.push(Span::styled(position, bar.fg(theme.muted)));

    if let Some(selection) = app.selection {
        let rows = selection.row_count();
        spans.push(Span::styled(
            format!("   {rows} {}", if rows == 1 { "line" } else { "lines" }),
            bar.fg(theme.orange),
        ));
    }

    if !pending_hint.is_empty() {
        spans.push(Span::styled(
            format!("   {pending_hint}"),
            bar.fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    }

    let comments = app
        .threads_by_path
        .values()
        .flatten()
        .filter(|thread| !thread.is_resolved)
        .count();
    if comments > 0 {
        spans.push(Span::styled(
            format!("   ◆ {comments}"),
            bar.fg(theme.purple),
        ));
    }

    if !app.drafts.is_empty() {
        spans.push(Span::styled(
            format!("   ✎ {}", app.drafts.len()),
            bar.fg(theme.orange),
        ));
    }

    if app.in_flight > 0 {
        spans.push(Span::styled(
            format!("   {}", SPINNER[app.loading_frame % SPINNER.len()]),
            bar.fg(theme.accent),
        ));
    }

    if !app.status.is_empty() {
        spans.push(Span::styled(
            format!("   {}", app.status),
            bar.fg(if app.is_status_alarming() {
                theme.danger
            } else {
                theme.dim
            }),
        ));
    }

    let exit_label = exit_hint.label();
    let keys: &[(&str, &str)] = match (app.mode, app.pane) {
        (Mode::Filter, _) => {
            &[("↑↓", "select"), ("↵", "apply"), ("esc", "cancel")]
        }
        (Mode::Search, _) => {
            &[("↑↓", "step"), ("↵", "accept"), ("esc", "cancel")]
        }
        (Mode::Help | Mode::Overview, _) if app.search.is_some() => {
            &[("n/N", "step"), ("/", "find"), ("esc", "close")]
        }
        (Mode::Help | Mode::Overview, _) => {
            &[("j/k", "scroll"), ("/", "find"), ("esc", "close")]
        }
        (Mode::CommandLine, _) => &[
            (":42", "line"),
            ("↑↓", "history"),
            ("↵", "run"),
            ("esc", "cancel"),
        ],
        (Mode::Submit, _) => &[
            ("⇥", "verdict"),
            ("↵", "send"),
            (
                "esc",
                if app
                    .submission
                    .as_ref()
                    .is_some_and(|submission| submission.is_discard_armed)
                {
                    "discard"
                } else {
                    "close"
                },
            ),
        ],
        (Mode::Insert, _) => &[
            ("↵", "save"),
            ("⇧↵", "newline"),
            (
                "esc",
                if app
                    .composer
                    .as_ref()
                    .is_some_and(|composer| composer.is_discard_armed)
                {
                    "discard"
                } else {
                    "close"
                },
            ),
        ],
        (Mode::Visual, _) => {
            &[("j/k", "extend"), ("c", "comment"), ("esc", "cancel")]
        }
        (Mode::Normal, Pane::Files) => &[
            ("j/k", "move"),
            ("↵", "open"),
            ("o", "description"),
            if app.file_filter.is_some() {
                ("/", "edit filter")
            } else {
                ("/", "filter")
            },
            ("q", exit_label),
        ],
        (Mode::Normal, Pane::Diff) if app.focused_draft().is_some() => &[
            ("j/k", "move"),
            (
                "↵",
                if app.expanded_card.is_some() {
                    "collapse"
                } else {
                    "expand"
                },
            ),
            ("e", "edit"),
            ("d", "discard"),
            ("esc", "code"),
        ],
        (Mode::Normal, Pane::Diff) if app.focused_card.is_some() => &[
            ("j/k", "move"),
            (
                "↵",
                if app.expanded_card.is_some() {
                    "collapse"
                } else {
                    "expand"
                },
            ),
            ("c", "reply"),
            ("R", "resolve"),
            ("esc", "code"),
        ],
        (Mode::Normal, Pane::Diff) if app.search.is_some() => &[
            ("n/N", "next match"),
            ("}", "next comment"),
            ("esc", "clear"),
        ],
        (Mode::Normal, Pane::Diff) if !app.drafts.is_empty() => &[
            ("c", "comment"),
            ("C", "file note"),
            ("e", "edit"),
            ("d", "discard"),
            ("s", "submit"),
        ],
        (Mode::Normal, Pane::Diff) => &[
            ("j/k", "move"),
            ("c", "comment"),
            ("/", "search"),
            ("}", "next comment"),
            ("q", exit_label),
        ],
    };

    // Claimed before the mode's own hints, since the bar truncates from the
    // tail. Dropped where `?` is not a key: inside a panel, or at a prompt.
    let reserved: &[(&str, &str)] =
        if app.mode.is_overlay() || app.mode.is_prompt() {
            &[]
        } else {
            &[("?", "keys")]
        };

    draw_status_bar(frame, area, spans, keys, reserved, theme);

    if matches!(app.mode, Mode::Search | Mode::CommandLine)
        && let Some(column) =
            prompt_column.filter(|column| *column < area.width as usize)
    {
        frame.set_cursor_position((area.x + column as u16, area.y));
    }
}

fn draw_hints(
    frame: &mut Frame,
    area: Rect,
    left_width: usize,
    keys: &[(&str, &str)],
    reserved: &[(&str, &str)],
    bar: Style,
    theme: Theme,
) {
    let claimed: usize = reserved.iter().map(|&pair| hint_width(pair)).sum();

    let available = (area.width as usize).saturating_sub(left_width + 2);
    let Some(budget) = available.checked_sub(claimed) else {
        return;
    };

    let mut hint_spans = Vec::with_capacity((keys.len() + reserved.len()) * 2);
    let mut used = 0;
    for &(key, label) in keys {
        let pair_width = hint_width((key, label));
        if used + pair_width > budget {
            break;
        }
        hint_spans.push(hint_key(key, bar, theme));
        hint_spans.push(hint_label(label, bar, theme));
        used += pair_width;
    }

    for &(key, label) in reserved {
        hint_spans.push(hint_key(key, bar, theme));
        hint_spans.push(hint_label(label, bar, theme));
        used += hint_width((key, label));
    }

    if used == 0 {
        return;
    }
    let hint_width = used;

    frame.render_widget(
        Paragraph::new(Line::from(hint_spans)).alignment(Alignment::Right),
        Rect {
            x: area.x + area.width.saturating_sub(hint_width as u16),
            width: hint_width as u16,
            ..area
        },
    );
}

fn hint_width((key, label): (&str, &str)) -> usize {
    text_width(key) + text_width(label) + 3
}

fn hint_key(key: &str, bar: Style, theme: Theme) -> Span<'static> {
    Span::styled(
        format!(" {key}"),
        bar.fg(theme.accent).add_modifier(Modifier::BOLD),
    )
}

fn hint_label(label: &str, bar: Style, theme: Theme) -> Span<'static> {
    Span::styled(format!(" {label} "), bar.fg(theme.dim))
}

fn draw_loading(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let theme = app.theme();
    let line = Line::from(vec![
        Span::styled(
            SPINNER[app.loading_frame % SPINNER.len()],
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  loading changes", Style::default().fg(theme.dim)),
    ]);

    frame.render_widget(
        Paragraph::new(line).alignment(Alignment::Center),
        Rect {
            y: area.y + area.height.saturating_sub(1) / 2,
            height: 1,
            ..area
        },
    );
}

fn draw_centered(frame: &mut Frame, area: Rect, message: &str, color: Color) {
    if area.is_empty() {
        return;
    }

    frame.render_widget(
        Paragraph::new(Line::styled(message, Style::default().fg(color)))
            .alignment(Alignment::Center),
        Rect {
            y: area.y + area.height.saturating_sub(1) / 2,
            height: 1,
            ..area
        },
    );
}
