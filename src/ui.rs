//! Drawing.
//!
//! Every function here reads app state and the frame's [`Layout`] and writes
//! only to the frame. Where things go and how tall they are was decided by the
//! layout, so nothing has to be discovered mid-render and written back.

use crate::app::draft::{Side, Sync};
use crate::app::mode::Mode;
use crate::app::review::ReviewEvent;
use crate::app::search::Query;
use crate::app::{App, Focus, OpenFile, Pane, Target, TreeRow};
use crate::layout::Layout;
use crate::layout::rows::{self, BodyRow, Connector, GUTTER, Row, ThreadState};
use crate::layout::tree::{self, Row as TreeNode};
use crate::model::{LineKind, ReviewThread};
use crate::renderer::{Theme, ThemeMode, markdown};
use crate::text::measure::{self, clip_text_to_budget, text_width, truncate};
use crate::text::wrap::{self, Fragment};
use devicons::{FileIcon, Theme as DeviconTheme, icon_for_file};
use ratatui::Frame;
use ratatui::layout::{Alignment, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};

/// The conversation mark and the space after it. Held on every row, directories
/// included, so the marks read as one column down the pane rather than moving
/// with the name beside them.
const MARKER_WIDTH: usize = 2;

/// A file's type icon and the space after it. Every glyph is one column, which
/// `every_tree_icon_is_one_column` holds to.
const ICON_WIDTH: usize = 2;

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub fn draw(frame: &mut Frame, app: &App, layout: &Layout, pending_hint: &str) {
    draw_header(frame, app, layout.header);

    if app.is_loading() {
        draw_loading(frame, app, layout.body);
        draw_bottom_bar(frame, app, layout, pending_hint);
        return;
    }

    if layout.files_pane.is_some() {
        draw_files(frame, app, layout);
    }
    draw_diff(frame, app, layout);

    draw_bottom_bar(frame, app, layout, pending_hint);
    draw_composer(frame, app, layout);
    draw_submit(frame, app, layout);
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

fn draw_files(frame: &mut Frame, app: &App, layout: &Layout) {
    let theme = app.theme();
    let is_focused = app.pane == Pane::Files;
    // Every file being under one directory is the common case for a review, so
    // the tree names it once here rather than in every row.
    let title = match (app.files.len(), layout.files.root()) {
        (0, _) => " Files ".to_string(),
        (count, None) => format!(" Files · {count} "),
        (count, Some(root)) => format!(" Files · {count} · {root} "),
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

    let query = app.tree_query();
    let cursor = app.tree_directory();
    let mut list: Vec<Line> = layout
        .files
        .window(height)
        .iter()
        .map(|row| match row {
            TreeNode::Directory {
                path,
                label,
                depth,
                files,
                unresolved,
                is_collapsed,
                ..
            } => directory_line(
                &DirectoryRow {
                    label,
                    depth: *depth,
                    files: *files,
                    unresolved: *unresolved,
                    is_collapsed: *is_collapsed,
                    is_selected: cursor == Some(&**path),
                },
                query,
                width,
                theme,
            ),
            TreeNode::File { index, depth, .. } => app
                .tree_row(*index)
                .map(|file| {
                    file_line(&file, query, *depth, width, theme, cursor)
                })
                .unwrap_or_default(),
        })
        .collect();

    if list.is_empty() && app.file_filter.is_some() {
        list.push(Line::styled(
            "  no matching files",
            Style::default().fg(theme.dim),
        ));
    }

    frame.render_widget(Paragraph::new(list), list_area);
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

    let mut spans = Vec::new();
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

/// Narrowest tree that shows churn beside a name and still leaves room to read
/// the name. Below it the two figures are worth less than the columns they cost.
const COUNTS_MIN_WIDTH: usize = 28;

/// The colour `devicons` names for a file type, which it gives as a CSS hex
/// string. An unreadable one falls back to the theme rather than to black.
fn icon_color(icon: FileIcon, theme: Theme) -> Color {
    let hex = icon.color.strip_prefix('#').unwrap_or(icon.color);
    let Ok(rgb) = u32::from_str_radix(hex, 16) else {
        return theme.muted;
    };

    Color::Rgb(
        ((rgb >> 16) & 0xff) as u8,
        ((rgb >> 8) & 0xff) as u8,
        (rgb & 0xff) as u8,
    )
}

/// A file's type icon, in the palette `devicons` picked for it.
fn file_icon(path: &str, theme: Theme) -> (char, Color) {
    let mode = match theme.mode {
        ThemeMode::Dark => DeviconTheme::Dark,
        ThemeMode::Light => DeviconTheme::Light,
    };
    let icon = icon_for_file(path, &Some(mode));

    (icon.icon, icon_color(icon, theme))
}

/// A directory heading as the tree will draw it.
struct DirectoryRow<'a> {
    label: &'a str,
    depth: usize,
    files: usize,
    unresolved: usize,
    is_collapsed: bool,
    is_selected: bool,
}

/// A directory, with what it holds. A folded one says how many files it is
/// keeping out of sight, since that is the only thing left to judge it by.
fn directory_line(
    row: &DirectoryRow<'_>,
    query: Option<Query<'_>>,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let base = if row.is_selected {
        Style::default()
            .bg(theme.cursor)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    // An open directory has its contents below it to say so. A folded one has
    // to say it in the row, and how much it is keeping out of sight.
    let fold = if row.is_collapsed {
        format!(" ▸ {}", row.files)
    } else {
        String::new()
    };
    let indent = row.depth * tree::INDENT;
    let budget =
        width.saturating_sub(indent + text_width(&fold) + MARKER_WIDTH);

    // Folding a directory must not fold away the reason to open it, so a shut
    // one carries the mark its files would have carried. An open one leaves the
    // column to them.
    let (marker, marker_color) = match (row.is_collapsed, row.unresolved) {
        (true, 0) => ("◇", theme.muted),
        (true, _) => ("◆", theme.purple),
        (false, _) => (" ", theme.dim),
    };

    let mut spans = vec![
        Span::styled(format!("{marker} "), base.fg(marker_color)),
        Span::styled(" ".repeat(indent), base),
    ];
    spans.extend(matched_spans(
        truncate(row.label, budget),
        query,
        base.fg(theme.muted).add_modifier(Modifier::BOLD),
        base.bg(theme.search),
    ));
    spans.push(Span::styled(fold, base.fg(theme.dim)));

    Line::from(spans)
}

fn file_line<'a>(
    row: &TreeRow<'a>,
    query: Option<Query<'_>>,
    depth: usize,
    width: usize,
    theme: Theme,
    // The heading the cursor left the open file to rest on, if it did. Only one
    // row in the tree carries the cursor bar.
    cursor_directory: Option<&str>,
) -> Line<'a> {
    let TreeRow {
        file,
        is_selected,
        threads,
        unresolved,
    } = *row;

    // A settled conversation still says something about the file, so it keeps a
    // hollow marker instead of disappearing from the tree.
    let (marker, marker_color) = match (unresolved, threads) {
        (0, 0) => (" ", theme.dim),
        (0, _) => ("◇", theme.muted),
        _ => ("◆", theme.purple),
    };
    // Churn is a glance, not a column of figures. Padding each side out to five
    // spent eleven columns of a pane whose whole job is naming files, and on a
    // narrow pane it goes entirely: which file this is outranks how much of it
    // changed.
    let counts = (width >= COUNTS_MIN_WIDTH).then(|| {
        (
            format!("+{}", file.additions),
            format!("-{}", file.deletions),
        )
    });
    let counts_width = counts
        .as_ref()
        .map_or(0, |(adds, dels)| adds.len() + dels.len() + 2);
    let indent = depth * tree::INDENT;
    let name_width =
        width.saturating_sub(counts_width + MARKER_WIDTH + indent + ICON_WIDTH);

    // Two things to say and no bar left to say one of them: the background is
    // where the cursor is, the weight is which file the diff is showing. They
    // are the same row until the cursor steps up onto a heading.
    let has_cursor = is_selected && cursor_directory.is_none();
    let base = match (has_cursor, is_selected) {
        (true, _) => Style::default()
            .bg(theme.cursor)
            .add_modifier(Modifier::BOLD),
        (false, true) => Style::default().add_modifier(Modifier::BOLD),
        _ => Style::default(),
    };

    // The heading above already named the directory, so a row carries its file
    // name alone.
    let name = file.path.rsplit('/').next().unwrap_or(&file.path);
    let name = truncate(name, name_width);
    let (glyph, glyph_color) = file_icon(&file.path, theme);
    let status_color = match file.status.as_str() {
        "added" => theme.success,
        "removed" => theme.danger,
        "renamed" => theme.warning,
        _ => theme.muted,
    };

    let pad = name_width.saturating_sub(text_width(&name));

    let hit = base.bg(theme.search);
    let mut spans = vec![
        Span::styled(format!("{marker} "), base.fg(marker_color)),
        Span::styled(" ".repeat(indent), base),
        Span::styled(format!("{glyph} "), base.fg(glyph_color)),
    ];
    spans.extend(matched_spans(name, query, base.fg(status_color), hit));
    spans.push(Span::styled(" ".repeat(pad), base));

    if let Some((adds, dels)) = counts {
        spans.extend([
            Span::styled(" ", base),
            Span::styled(adds, base.fg(theme.success)),
            Span::styled(" ", base),
            Span::styled(dels, base.fg(theme.danger)),
        ]);
    }

    Line::from(spans)
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
            Row::Draft { draft } => {
                draft_line(&open, focus, *draft, width, theme)
            }
            Row::Heading { state, count } => {
                heading_line(*state, *count, width, theme)
            }
            Row::Hidden {
                state,
                count,
                is_tail,
            } => hidden_line(*state, *count, *is_tail, width, theme),
            Row::Summary {
                thread,
                state,
                connector,
                has_state_label,
            } => summary_line(
                focus,
                &open.threads[*thread],
                *state,
                *connector,
                *has_state_label,
                width,
                theme,
            ),
            Row::Body { index, is_last } => {
                body_row(layout.rows.body(*index), *is_last, width, theme)
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), area);
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
    focus: Focus<'_>,
    thread: &ReviewThread,
    state: ThreadState,
    connector: Connector,
    has_state_label: bool,
    width: usize,
    theme: Theme,
) -> Line<'static> {
    let indent = card_indent(width);
    let card_width = width.saturating_sub(indent);
    let is_focused = focus.is_thread_focused(&thread.id);
    let is_expanded = focus.is_thread_expanded(&thread.id);

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
) -> Line<'static> {
    let Some(body) = body else {
        return Line::default();
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
            ("}", "comments"),
            if app.file_filter.is_some() {
                ("/", "edit filter")
            } else {
                ("/", "filter")
            },
        ],
        (Mode::Normal, Pane::Diff) if app.focused_draft().is_some() => &[
            (
                "j/k",
                if app.expanded_card.is_some() {
                    "scroll"
                } else {
                    "move"
                },
            ),
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
            (
                "j/k",
                if app.expanded_card.is_some() {
                    "scroll"
                } else {
                    "move"
                },
            ),
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
