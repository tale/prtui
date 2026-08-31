//! File-tree rendering.

use super::{draw_centered, matched_spans, pane_block};
use crate::app::mode::Mode;
use crate::app::search::Query;
use crate::app::{App, Pane, TreeRow};
use crate::layout::Layout;
use crate::layout::tree::{self, Row as TreeNode};
use crate::renderer::{Theme, ThemeMode};
use crate::text::measure::{self, text_width, truncate};
use devicons::{FileIcon, Theme as DeviconTheme, icon_for_file};
use ratatui::Frame;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Borders, Paragraph};

/// The conversation mark and the space after it. Held on every row, directories
/// included, so the marks read as one column down the pane rather than moving
/// with the name beside them.
const MARKER_WIDTH: usize = 2;

/// A file's type icon and the space after it. Every glyph is one column, which
/// `every_tree_icon_is_one_column` holds to.
const ICON_WIDTH: usize = 2;

pub(super) fn draw(frame: &mut Frame, app: &App, layout: &Layout) {
    let Some(pane) = layout.files_pane else {
        return;
    };

    let theme = app.theme();
    let is_focused = app.pane == Pane::Files;
    // Every file being under one directory is the common case for a review, so
    // the tree names it once here rather than in every row.
    let title = match (app.files.len(), layout.files.root()) {
        (0, _) => " Files ".to_string(),
        (count, None) => format!(" Files · {count} "),
        (count, Some(root)) => format!(" Files · {count} · {root} "),
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
                    is_selected: is_focused && cursor == Some(&**path),
                },
                query,
                width,
                theme,
            ),
            TreeNode::File { index, depth, .. } => app
                .tree_row(*index)
                .map(|file| {
                    file_line(
                        &file,
                        query,
                        *depth,
                        width,
                        theme,
                        FileRowFocus {
                            pane: is_focused,
                            directory: cursor,
                        },
                    )
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

    let name = matched_spans(
        truncate(row.label, budget),
        query,
        base.fg(theme.muted).add_modifier(Modifier::BOLD),
        base.bg(theme.search),
    );
    let mut spans = Vec::with_capacity(name.len() + 3);
    spans.extend([
        Span::styled(format!("{marker} "), base.fg(marker_color)),
        Span::styled(" ".repeat(indent), base),
    ]);
    spans.extend(name);
    spans.push(Span::styled(fold, base.fg(theme.dim)));

    Line::from(spans)
}

/// Where the tree's cursor is, as one file row needs to know it.
#[derive(Clone, Copy)]
struct FileRowFocus<'a> {
    /// Whether the tree is the pane holding the keys. The bar is what says
    /// where typing lands, so only the focused pane draws one.
    pane: bool,
    /// The heading the cursor left the open file to rest on, if it did. Only
    /// one row in the tree carries the cursor bar.
    directory: Option<&'a str>,
}

fn file_line<'a>(
    row: &TreeRow<'a>,
    query: Option<Query<'_>>,
    depth: usize,
    width: usize,
    theme: Theme,
    focus: FileRowFocus<'_>,
) -> Line<'a> {
    let TreeRow {
        file,
        is_selected,
        is_viewed,
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
    // are the same row until the cursor steps up onto a heading — or until the
    // diff takes the keys, which leaves the weight alone to say it.
    let has_cursor = focus.pane && is_selected && focus.directory.is_none();
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
    // A file already read through takes a tick in place of its type icon and
    // loses the color it earned for being added or removed: what it was is no
    // longer the thing to look at, and the icon column is already there.
    let (glyph, glyph_color) = if is_viewed {
        ('✓', theme.success)
    } else {
        file_icon(&file.path, theme)
    };
    let status_color = match (is_viewed, file.status.as_str()) {
        (true, _) => theme.dim,
        (_, "added") => theme.success,
        (_, "removed") => theme.danger,
        (_, "renamed") => theme.warning,
        _ => theme.muted,
    };

    let pad = name_width.saturating_sub(text_width(&name));

    let hit = base.bg(theme.search);
    let name = matched_spans(name, query, base.fg(status_color), hit);
    let trailing = if counts.is_some() { 8 } else { 4 };
    let mut spans = Vec::with_capacity(name.len() + trailing);
    spans.extend([
        Span::styled(format!("{marker} "), base.fg(marker_color)),
        Span::styled(" ".repeat(indent), base),
        Span::styled(format!("{glyph} "), base.fg(glyph_color)),
    ]);
    spans.extend(name);
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
