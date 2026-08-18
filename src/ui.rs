//! Drawing.
//!
//! Every function here reads app state and the frame's [`Layout`] and writes
//! only to the frame. Where things go and how tall they are was decided by the
//! layout, so nothing has to be discovered mid-render and written back.

use crate::app::draft::{Draft, Side};
use crate::app::mode::Mode;
use crate::app::review::ReviewEvent;
use crate::app::{App, Pane, Target};
use crate::images::Placement;
use crate::layout::Layout;
use crate::layout::measure::{self, clip_text_to_budget, text_width, truncate};
use crate::layout::rows::{
    self, BodyRow, Connector, GUTTER, ImageSlice, Row, ThreadState,
};
use crate::layout::wrap::{self, Fragment};
use crate::model::{LineKind, ReviewThread};
use crate::renderer::{Theme, markdown};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// One rendered row, plus the horizontal slice of an image it stands in for.
/// The row itself stays blank: the terminal paints over it after the frame.
struct PaneRow<'a> {
    line: Line<'a>,
    image: Option<ImageSlice>,
}

impl<'a> PaneRow<'a> {
    const fn text(line: Line<'a>) -> Self {
        Self { line, image: None }
    }
}

/// Returns where this frame's images land, which the caller turns into escape
/// sequences and writes inside the same synchronized update as the cells.
pub fn draw(
    frame: &mut Frame,
    app: &App,
    layout: &Layout,
    pending_hint: &str,
) -> Vec<Placement> {
    draw_header(frame, app, layout.header);

    if app.is_loading() {
        draw_loading(frame, app, layout.body);
        draw_bottom_bar(frame, app, layout, pending_hint);
        return Vec::new();
    }

    if layout.files_pane.is_some() {
        draw_files(frame, app, layout);
    }
    let placements = draw_diff(frame, app, layout);

    draw_bottom_bar(frame, app, layout, pending_hint);
    draw_composer(frame, app, layout);
    draw_submit(frame, app, layout);

    placements
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

            vec![
                Span::styled(
                    label,
                    Style::default()
                        .bg(color)
                        .fg(theme.ink)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" #{} ", pr.number),
                    Style::default()
                        .fg(theme.warning)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate(&pr.title, area.width as usize / 2),
                    Style::default()
                        .fg(theme.heading)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {} → {}", pr.head_ref, pr.base_ref),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(
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

fn draw_files(frame: &mut Frame, app: &App, layout: &Layout) {
    let theme = app.theme();
    let is_focused = app.pane == Pane::Files;
    let title = if app.files.is_empty() {
        " Files ".to_string()
    } else {
        format!(" Files · {} ", app.files.len())
    };

    let Some(pane) = layout.files_pane else {
        return;
    };
    frame.render_widget(
        pane_block(title, is_focused, theme)
            .borders(Borders::TOP | Borders::RIGHT),
        pane,
    );

    let Some(list_area) = layout.files_list else {
        return;
    };

    if app.files.is_empty() {
        draw_centered(frame, list_area, app.files_placeholder(), theme.dim);
        return;
    }

    if let (Some(filter), Some(prompt)) =
        (app.file_filter.as_ref(), layout.files_prompt)
    {
        let query = &filter.lines()[0];
        let (_, cursor_byte) = filter.cursor();
        let budget = prompt.width.saturating_sub(2) as usize;
        let cursor_column = text_width(&query[..cursor_byte]);
        let first_column =
            cursor_column.saturating_sub(budget.saturating_sub(1));

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" /", Style::default().fg(theme.accent)),
                Span::styled(
                    measure::window(query, first_column, budget),
                    Style::default().fg(theme.heading),
                ),
            ])),
            prompt,
        );

        if app.mode == Mode::Filter {
            frame.set_cursor_position((
                prompt.x
                    + 2
                    + cursor_column.saturating_sub(first_column) as u16,
                prompt.y,
            ));
        }
    }

    let height = list_area.height as usize;
    let width = list_area.width as usize;
    let matches = app.filtered_file_indices();
    let selected_position = matches
        .iter()
        .position(|&index| index == app.selected_file)
        .unwrap_or(0);
    let start = selected_position
        .saturating_sub(height / 2)
        .min(matches.len().saturating_sub(height));

    let mut list: Vec<Line> = matches
        .iter()
        .skip(start)
        .take(height)
        .map(|&index| file_line(app, index, width, theme))
        .collect();

    if list.is_empty() && app.file_filter.is_some() {
        list.push(Line::styled(
            "  no matching files",
            Style::default().fg(theme.dim),
        ));
    }

    frame.render_widget(Paragraph::new(list), list_area);
}

fn file_line(app: &App, index: usize, width: usize, theme: Theme) -> Line<'_> {
    let file = &app.files[index];
    let is_selected = index == app.selected_file;
    let threads = app
        .threads_by_path
        .get(&file.path)
        .map_or(&[][..], Vec::as_slice);
    let unresolved = threads.iter().filter(|t| !t.is_resolved).count();

    // A settled conversation still says something about the file, so it keeps a
    // hollow marker instead of disappearing from the tree.
    let (marker, marker_color) = match (unresolved, threads.len()) {
        (0, 0) => ("  ".to_string(), theme.dim),
        (0, total) => (format!(" ◇ {total}"), theme.muted),
        (open, _) => (format!(" ◆ {open}"), theme.purple),
    };
    let adds = format!("+{}", file.additions);
    let dels = format!("-{}", file.deletions);

    let counts_width = adds.len().max(5) + dels.len().max(5) + 1;
    let name_width =
        width.saturating_sub(counts_width + text_width(&marker) + 2);

    let base = if is_selected {
        Style::default()
            .bg(theme.cursor)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let (dir, name) = measure::split_path(&file.path, name_width);
    let status_color = match file.status.as_str() {
        "added" => theme.success,
        "removed" => theme.danger,
        "renamed" => theme.warning,
        _ => theme.muted,
    };

    let pad = name_width.saturating_sub(text_width(&dir) + text_width(&name));

    Line::from(vec![
        Span::styled(
            if is_selected { " ▍" } else { "  " },
            base.fg(theme.accent),
        ),
        Span::styled(dir, base.fg(theme.dim)),
        Span::styled(name, base.fg(status_color)),
        Span::styled(" ".repeat(pad), base),
        Span::styled(marker, base.fg(marker_color)),
        Span::styled(format!("{adds:>5}"), base.fg(theme.success)),
        Span::styled(format!(" {dels:>5}"), base.fg(theme.danger)),
    ])
}

fn draw_diff(frame: &mut Frame, app: &App, layout: &Layout) -> Vec<Placement> {
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

    if app.current_file().is_none() {
        draw_centered(frame, area, "no diff selected", theme.dim);
        return Vec::new();
    }

    let width = area.width as usize;
    let threads = app.file_threads();
    let drafts: Vec<&Draft> = app
        .drafts
        .iter()
        .filter(|draft| Some(draft.path.as_str()) == app.current_path())
        .collect();

    let rendered: Vec<PaneRow<'_>> = layout
        .rows
        .window(app.diff_scroll, area.height as usize)
        .iter()
        .map(|row| match row {
            Row::Code { source, fragment } => PaneRow::text(code_line(
                app, *source, *fragment, &drafts, threads, width, theme,
            )),
            Row::FileDraft => PaneRow::text(file_draft_line(app, width, theme)),
            Row::Heading { state, count } => {
                PaneRow::text(heading_line(*state, *count, width, theme))
            }
            Row::Hidden {
                state,
                count,
                is_tail,
            } => PaneRow::text(hidden_line(
                *state, *count, *is_tail, width, theme,
            )),
            Row::Summary {
                thread,
                state,
                connector,
                has_state_label,
            } => PaneRow::text(summary_line(
                app,
                &threads[*thread],
                *state,
                *connector,
                *has_state_label,
                width,
                theme,
            )),
            Row::Body { index, is_last } => {
                body_row(layout.rows.body(*index), *is_last, width, theme)
            }
        })
        .collect();

    let (lines, placements) = split_image_rows(rendered, area);
    frame.render_widget(Paragraph::new(lines), area);
    placements
}

fn code_line<'a>(
    app: &'a App,
    index: usize,
    fragment: Fragment,
    drafts: &[&Draft],
    threads: &[ReviewThread],
    width: usize,
    theme: Theme,
) -> Line<'a> {
    let Some(line) = app.current_file().and_then(|file| file.lines.get(index))
    else {
        return Line::default();
    };

    let is_cursor = app.pane == Pane::Diff
        && index == app.cursor
        && app.focused_thread.is_none();
    let is_selected = app.selection.is_some_and(|s| s.contains(index));

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

    let has_thread = threads.iter().any(|thread| {
        !thread.is_outdated && thread.anchors_to(line) && !thread.is_resolved
    });
    let has_draft = drafts
        .iter()
        .any(|draft| draft.rows().is_some_and(|rows| rows.contains(&index)));

    let (marker, marker_color) = match (has_draft, has_thread) {
        (true, _) => (" ✎", theme.orange),
        (false, true) => (" ◆", theme.purple),
        _ => ("  ", theme.dim),
    };

    // The bar runs down every row of a folded line so the whole of it reads as
    // one block, but the numbers, marker and sigil belong to its first row only.
    let mut spans = vec![Span::styled(
        if is_cursor || is_selected { "▍" } else { " " },
        Style::default().bg(bg).fg(theme.accent),
    )];

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
        spans.push(Span::styled(
            " ".repeat(GUTTER - 1),
            Style::default().bg(bg),
        ));
    }

    let styled = app.highlighted();
    let colored: Vec<Piece> =
        match styled.and_then(|s| s.get(index)).filter(|s| !s.is_empty()) {
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
    for piece in split_by_matches(colored, &app.line_match_ranges(index)) {
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

/// Splits rendered rows into the text the buffer draws and the image placements
/// the terminal paints over them, merging each image's rows into one placement.
fn split_image_rows(
    rendered: Vec<PaneRow<'_>>,
    area: Rect,
) -> (Vec<Line<'_>>, Vec<Placement>) {
    let mut lines = Vec::with_capacity(rendered.len());
    let mut placements: Vec<Placement> = Vec::new();

    for (index, row) in rendered.into_iter().enumerate() {
        lines.push(row.line);

        let Some(slice) = row.image else {
            continue;
        };
        let screen_row = area.y + index as u16;
        let continues = placements.last().is_some_and(|last| {
            last.url == slice.url
                && last.row + last.rows == screen_row
                && last.skip_rows + last.rows == slice.row_index
        });

        match placements.last_mut() {
            Some(last) if continues => last.rows += 1,
            _ => placements.push(Placement {
                url: slice.url,
                column: area.x + slice.column,
                row: screen_row,
                cols: slice.cols,
                rows: 1,
                skip_rows: slice.row_index,
                total_rows: slice.total_rows,
            }),
        }
    }

    (lines, placements)
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

/// The pending remark about the file as a whole. It has no line to sit under, so
/// it leads the pane and carries the draft marker the gutter uses elsewhere.
fn file_draft_line(app: &App, width: usize, theme: Theme) -> Line<'static> {
    let indent = card_indent(width);
    let label = "file note";
    let body = app.file_draft().unwrap_or_default();
    let summary = comment_summary(body, theme);
    let budget = width
        .saturating_sub(indent)
        .saturating_sub(text_width(label) + 4);

    thread_card_line(
        indent,
        width,
        vec![
            rows::card_span("✎ ", theme.orange, Modifier::BOLD, theme),
            rows::card_span(label, theme.heading, Modifier::BOLD, theme),
            rows::card_span("  ", theme.muted, Modifier::empty(), theme),
            rows::card_span(
                truncate(&summary, budget),
                theme.code,
                Modifier::empty(),
                theme,
            ),
        ],
        theme,
        false,
    )
}

fn heading_line(
    state: ThreadState,
    count: usize,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let indent = card_indent(width);
    let heading =
        format!("{} {count} {} threads", state.marker(), state.label());

    thread_card_line(
        indent,
        width,
        vec![rows::card_span(
            truncate(&heading, width.saturating_sub(indent)),
            state_color(state, theme),
            Modifier::BOLD,
            theme,
        )],
        theme,
        false,
    )
}

fn hidden_line(
    state: ThreadState,
    count: usize,
    is_tail: bool,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let (connector, label) = if is_tail {
        ("└ ", format!("… {count} more"))
    } else {
        ("├ ", format!("… {count} earlier"))
    };

    thread_card_line(
        card_indent(width),
        width,
        vec![
            rows::card_span(
                connector,
                state_color(state, theme),
                Modifier::BOLD,
                theme,
            ),
            rows::card_span(label, theme.muted, Modifier::empty(), theme),
        ],
        theme,
        false,
    )
}

fn summary_line(
    app: &App,
    thread: &ReviewThread,
    state: ThreadState,
    connector: Connector,
    has_state_label: bool,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let indent = card_indent(width);
    let card_width = width.saturating_sub(indent);
    let is_focused = app.focused_thread.as_deref() == Some(&thread.id);
    let is_expanded = app.is_thread_expanded(&thread.id);

    // An expanded card gives its one line to the conversation's shape instead of
    // repeating the first comment, which is about to be printed underneath.
    let author = if is_expanded {
        String::new()
    } else {
        thread.comments.first().map_or_else(
            || "review thread".into(),
            |comment| format!("@{}", comment.author),
        )
    };
    let separator = if is_expanded { "" } else { "  " };
    let summary = if is_expanded {
        comment_count(thread.comments.len())
    } else {
        thread.comments.first().map_or_else(
            || "no comment body".into(),
            |comment| comment_summary(&comment.body, theme),
        )
    };

    let replies = thread.comments.len().saturating_sub(1);
    let mut suffix = match (is_expanded, replies) {
        (true, _) | (_, 0) => String::new(),
        (false, 1) => " · 1 reply".into(),
        (false, count) => format!(" · {count} replies"),
    };
    if has_state_label {
        suffix.push_str(" · ");
        suffix.push_str(state.label());
    }

    let prefix = match connector {
        Connector::Only => format!("{} ", state.marker()),
        Connector::Branch => "├ ".to_string(),
        Connector::Last => "└ ".to_string(),
    };
    let prefix = if is_expanded {
        format!("{prefix}▾ ")
    } else {
        prefix
    };

    let fixed = text_width(&prefix)
        + text_width(&author)
        + text_width(separator)
        + text_width(&suffix);
    let summary = truncate(&summary, card_width.saturating_sub(fixed));

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
            rows::card_span(author, theme.heading, Modifier::BOLD, theme),
            rows::card_span(separator, theme.muted, Modifier::empty(), theme),
            rows::card_span(summary, theme.code, Modifier::empty(), theme),
            rows::card_span(suffix, theme.muted, Modifier::empty(), theme),
        ],
        theme,
        is_focused,
    )
}

fn body_row(
    body: Option<&BodyRow>,
    is_last: bool,
    width: usize,
    theme: Theme,
) -> PaneRow<'static> {
    let Some(body) = body else {
        return PaneRow::text(Line::default());
    };

    let mut spans = body.spans.clone();
    spans.insert(
        0,
        rows::card_span(
            if is_last { "└  " } else { "│  " },
            theme.purple,
            Modifier::empty(),
            theme,
        ),
    );

    PaneRow {
        line: thread_card_line(card_indent(width), width, spans, theme, false),
        image: body.image.clone(),
    }
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
    let gutter = if is_focused && indent > 0 {
        vec![
            Span::raw(" ".repeat(indent - 1)),
            Span::styled("▍", Style::default().fg(theme.accent)),
        ]
    } else {
        vec![Span::raw(" ".repeat(indent))]
    };

    for span in gutter.into_iter().rev() {
        spans.insert(0, span);
    }
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
            let side = |side: Side| side.as_api().to_lowercase();

            // A span that crosses sides counts in two files at once, so both
            // ends have to name the one they belong to.
            if anchor.start_side == anchor.side {
                let span = if anchor.start_line == anchor.end_line {
                    format!("{}", anchor.end_line)
                } else {
                    format!("{}-{}", anchor.start_line, anchor.end_line)
                };

                format!(" {verb} · {name}:{span} · {} ", side(anchor.side))
            } else {
                format!(
                    " {verb} · {name}:{} {} → {} {} ",
                    anchor.start_line,
                    side(anchor.start_side),
                    anchor.end_line,
                    side(anchor.side)
                )
            }
        }
    };

    let block = docked_block(title, theme.orange);
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

    let wrapped = wrap::Wrapped::new(lines, area.width as usize);
    let (line, byte) = editor.cursor();
    let (cursor_row, cursor_column) = wrapped.locate(line, byte);

    // Folding means the cursor can only ever leave the viewport vertically.
    let height = area.height as usize;
    let first_row = cursor_row.saturating_sub(height.saturating_sub(1));
    let visible: Vec<Line> = wrapped
        .rows()
        .iter()
        .skip(first_row)
        .take(height)
        .map(|row| {
            Line::styled(wrapped.text(*row), Style::default().fg(theme.code))
        })
        .collect();

    frame.render_widget(Paragraph::new(visible), area);
    frame.set_cursor_position((
        area.x + cursor_column as u16,
        area.y + (cursor_row - first_row) as u16,
    ));
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

    let block = docked_block(title, theme.accent);
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

fn draw_bottom_bar(
    frame: &mut Frame,
    app: &App,
    layout: &Layout,
    pending_hint: &str,
) {
    let area = layout.status;
    let theme = app.theme();
    let bar = Style::default().bg(theme.hunk);
    let mode_bg = match app.mode {
        Mode::Normal => theme.accent,
        Mode::Visual => theme.orange,
        Mode::Insert => theme.success,
        Mode::Filter => theme.purple,
        Mode::Search => theme.warning,
        Mode::Submit => theme.heading,
    };

    let pane = match app.pane {
        Pane::Files => " files",
        Pane::Diff => " diff",
    };

    let show_search_position = app.search.is_some() && app.pane == Pane::Diff;
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
        (false, true, _) => {
            let matches = app.filtered_file_indices();
            let selected = matches
                .iter()
                .position(|&index| index == app.selected_file)
                .map_or(0, |position| position + 1);
            format!("  {selected}/{} matches", matches.len())
        }
        (false, false, Some(file)) => format!(
            "  {}/{} · {}/{}",
            app.selected_file + 1,
            app.files.len(),
            (app.cursor + 1).min(file.lines.len().max(1)),
            file.lines.len()
        ),
        (false, false, None) => String::new(),
    };

    let mut spans = vec![
        Span::styled(
            app.mode.label(),
            Style::default()
                .bg(mode_bg)
                .fg(theme.ink)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(pane, bar.fg(theme.dim)),
    ];

    let mut search_column = None;
    if let Some(editor) = app.search.as_ref() {
        let query = editor.lines()[0].clone();
        let (_, cursor_byte) = editor.cursor();

        search_column = Some(
            Line::from(spans.clone()).width()
                + 3
                + text_width(&query[..cursor_byte]),
        );
        spans.push(Span::styled("  /", bar.fg(theme.warning)));
        spans.push(Span::styled(query, bar.fg(theme.heading)));
    }

    spans.push(Span::styled(position, bar.fg(theme.muted)));

    if let Some(selection) = app.selection {
        spans.push(Span::styled(
            format!("   {} lines", selection.row_count()),
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

    frame.render_widget(
        Paragraph::new(Span::styled(" ".repeat(area.width as usize), bar)),
        area,
    );

    let left = Line::from(spans);
    let left_width = left.width();
    frame.render_widget(Paragraph::new(left), area);

    if app.mode == Mode::Search
        && let Some(column) =
            search_column.filter(|column| *column < area.width as usize)
    {
        frame.set_cursor_position((area.x + column as u16, area.y));
    }

    draw_key_hints(frame, app, area, left_width, bar, theme);
}

fn draw_key_hints(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    left_width: usize,
    bar: Style,
    theme: Theme,
) {
    let keys: &[(&str, &str)] = match (app.mode, app.pane) {
        (Mode::Filter, _) => {
            &[("↑↓", "select"), ("↵", "apply"), ("esc", "cancel")]
        }
        (Mode::Search, _) => {
            &[("↑↓", "step"), ("↵", "accept"), ("esc", "cancel")]
        }
        (Mode::Submit, _) => {
            &[("⇥", "verdict"), ("↵", "send"), ("esc", "cancel")]
        }
        (Mode::Insert, _) => {
            &[("↵", "save"), ("⇧↵", "newline"), ("esc", "cancel")]
        }
        (Mode::Visual, _) => {
            &[("j/k", "extend"), ("c", "comment"), ("esc", "cancel")]
        }
        (Mode::Normal, Pane::Files) => &[
            ("j/k", "move"),
            ("↵", "open"),
            ("}", "comments"),
            if app.file_filter.is_some() {
                ("/", "edit filter")
            } else {
                ("/", "filter")
            },
        ],
        (Mode::Normal, Pane::Diff) if app.focused_thread.is_some() => &[
            (
                "j/k",
                if app.expanded_thread.is_some() {
                    "scroll"
                } else {
                    "move"
                },
            ),
            (
                "↵",
                if app
                    .focused_thread
                    .as_deref()
                    .is_some_and(|id| app.is_thread_expanded(id))
                {
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
            ("e", "edit"),
            ("d", "discard"),
            ("s", "submit"),
        ],
        (Mode::Normal, Pane::Diff) => &[
            ("j/k", "move"),
            ("/", "search"),
            ("}", "next comment"),
            ("c", "comment"),
            ("C", "file note"),
        ],
    };

    let available = (area.width as usize).saturating_sub(left_width + 2);
    let mut hint_spans = Vec::new();
    let mut hint_width = 0;
    for &(key, label) in keys {
        let pair_width = text_width(key) + text_width(label) + 3;
        if hint_width + pair_width > available {
            break;
        }
        hint_spans.push(Span::styled(
            format!(" {key}"),
            bar.fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
        hint_spans.push(Span::styled(format!(" {label} "), bar.fg(theme.dim)));
        hint_width += pair_width;
    }

    if hint_width == 0 {
        return;
    }

    frame.render_widget(
        Paragraph::new(Line::from(hint_spans)).alignment(Alignment::Right),
        Rect {
            x: area.x + area.width.saturating_sub(hint_width as u16),
            width: hint_width as u16,
            ..area
        },
    );
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
