use crate::app::draft::{Draft, Side};
use crate::app::mode::Mode;
use crate::app::{App, Pane};
use crate::model::LineKind;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use std::borrow::Cow;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

const GUTTER: usize = 13;

pub fn draw(frame: &mut Frame, app: &mut App, pending_hint: &str) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, app, rows[0]);

    if app.is_loading() {
        draw_loading(frame, app, rows[1]);
        draw_bottom_bar(frame, app, pending_hint, rows[2]);
        return;
    }

    let diff_area = if app.is_files_visible {
        let width = files_width(rows[1].width);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(width), Constraint::Min(20)])
            .split(rows[1]);

        draw_files(frame, app, cols[0]);
        draw_diff(frame, app, cols[1]);
        cols[1]
    } else {
        draw_diff(frame, app, rows[1]);
        rows[1]
    };

    draw_bottom_bar(frame, app, pending_hint, rows[2]);
    draw_composer(frame, app, diff_area);
}

/// Roughly a quarter of the terminal, clamped so the tree neither crowds the
/// diff on a narrow window nor sprawls on a wide one.
fn files_width(total: u16) -> u16 {
    (total / 4).clamp(22, 34).min(total.saturating_sub(20))
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
                    truncate_right(&pr.title, area.width as usize / 2),
                    Style::default()
                        .fg(theme.heading)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {} → {}", pr.head_ref, pr.base_ref),
                    Style::default().fg(theme.muted),
                ),
                Span::styled(format!("  @{}", pr.author), Style::default().fg(theme.dim)),
            ]
        }
    };

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_files(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let is_focused = app.pane == Pane::Files;
    let title = if app.files.is_empty() {
        " Files ".to_string()
    } else {
        format!(" Files · {} ", app.files.len())
    };
    let block = Block::default()
        .borders(Borders::TOP | Borders::RIGHT)
        .border_style(Style::default().fg(if is_focused { theme.accent } else { theme.dim }))
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
        ));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if app.files.is_empty() {
        draw_empty_pane(frame, app, inner, "no changed files");
        return;
    }

    let list_area = if let Some(filter) = app.file_filter.as_ref() {
        if inner.height == 0 {
            return;
        }

        let query = &filter.lines()[0];
        let (_, cursor_byte) = filter.cursor();
        let prompt_width = inner.width.saturating_sub(2) as usize;
        let cursor_column = terminal_width(&query[..cursor_byte]);
        let first_column = cursor_column.saturating_sub(prompt_width.saturating_sub(1));
        let text = clip_window(query, first_column, prompt_width);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" /", Style::default().fg(theme.accent)),
                Span::styled(text, Style::default().fg(theme.heading)),
            ])),
            Rect { height: 1, ..inner },
        );
        if app.mode == Mode::Filter {
            frame.set_cursor_position((
                inner.x + 2 + cursor_column.saturating_sub(first_column) as u16,
                inner.y,
            ));
        }

        Rect {
            y: inner.y + 1,
            height: inner.height - 1,
            ..inner
        }
    } else {
        inner
    };

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

    let mut rows: Vec<Line> = matches
        .iter()
        .skip(start)
        .take(height)
        .map(|&index| {
            let file = &app.files[index];
            let is_selected = index == app.selected_file;
            let unresolved = app
                .threads_by_path
                .get(&file.path)
                .map(|list| list.iter().filter(|t| !t.is_resolved).count())
                .unwrap_or(0);

            let marker = if unresolved > 0 {
                format!(" ▤{unresolved}")
            } else {
                "  ".into()
            };
            let adds = format!("+{}", file.additions);
            let dels = format!("-{}", file.deletions);

            let counts_width = adds.len().max(5) + dels.len().max(5) + 1;
            let name_width = width.saturating_sub(counts_width + marker.chars().count() + 2);

            let base = if is_selected {
                Style::default()
                    .bg(theme.cursor)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let (dir, name) = split_path(&file.path, name_width);
            let status_color = match file.status.as_str() {
                "added" => theme.success,
                "removed" => theme.danger,
                "renamed" => theme.warning,
                _ => theme.muted,
            };

            let pad = name_width.saturating_sub(dir.chars().count() + name.chars().count());

            Line::from(vec![
                Span::styled(if is_selected { " ▍" } else { "  " }, base.fg(theme.accent)),
                Span::styled(dir, base.fg(theme.dim)),
                Span::styled(name, base.fg(status_color)),
                Span::styled(" ".repeat(pad), base),
                Span::styled(marker, base.fg(theme.purple)),
                Span::styled(format!("{adds:>5}"), base.fg(theme.success)),
                Span::styled(format!(" {dels:>5}"), base.fg(theme.danger)),
            ])
        })
        .collect();

    if rows.is_empty() && app.file_filter.is_some() {
        rows.push(Line::styled(
            "  no matching files",
            Style::default().fg(theme.dim),
        ));
    }

    frame.render_widget(Paragraph::new(rows), list_area);
}

fn draw_diff(frame: &mut Frame, app: &App, area: Rect) {
    let theme = app.theme();
    let is_focused = app.pane == Pane::Diff;
    let title = app.current_file().map_or_else(
        || " Diff ".to_string(),
        |file| {
            let comments = app.threads_by_path.get(&file.path).map_or(0, |threads| {
                threads.iter().filter(|thread| !thread.is_resolved).count()
            });
            let suffix = if comments == 0 {
                format!("  +{} -{}", file.additions, file.deletions)
            } else {
                format!("  ▤ {comments}  +{} -{}", file.additions, file.deletions)
            };
            let available = area.width.saturating_sub(4) as usize;
            format!(
                " {}{} ",
                truncate_right(
                    &file.path,
                    available.saturating_sub(terminal_width(&suffix)),
                ),
                suffix
            )
        },
    );
    let block = Block::default()
        .borders(Borders::TOP)
        .border_style(Style::default().fg(if is_focused { theme.accent } else { theme.dim }))
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
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(file) = app.current_file() else {
        draw_empty_pane(frame, app, inner, "no diff selected");
        return;
    };

    let height = inner.height as usize;
    let width = inner.width as usize;
    let threads = app.threads_by_path.get(&file.path);
    let styled = app.highlighted();

    let drafts: Vec<&Draft> = app.drafts.iter().filter(|d| d.path == file.path).collect();

    // Only the visible slice is ever converted to spans; a 7k-line diff costs
    // the same to render as a 40-line one.
    let rows: Vec<Line> = file
        .lines
        .iter()
        .enumerate()
        .skip(app.diff_scroll)
        .take(height)
        .map(|(index, line)| {
            let is_cursor = is_focused && index == app.cursor;
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

            // Selected rows keep their add/remove identity and are shifted instead
            // of flattened; the left bar is what makes the span read as contiguous.
            let bg = match (is_selected, is_cursor) {
                (true, _) => theme.selection_background(base_bg),
                (false, true) => theme.cursor_background(base_bg),
                _ => base_bg,
            };

            let has_thread = line.new_line.is_some_and(|n| {
                threads.is_some_and(|list| list.iter().any(|t| t.line == Some(n) && !t.is_resolved))
            });

            let has_draft = drafts.iter().any(|d| match d.side {
                Side::Right => line
                    .new_line
                    .is_some_and(|n| d.covers(&file.path, n, Side::Right)),
                Side::Left => line
                    .old_line
                    .is_some_and(|n| d.covers(&file.path, n, Side::Left)),
            });

            let (marker, marker_color) = match (has_draft, has_thread) {
                (true, _) => (" ✎", theme.orange),
                (false, true) => (" ◆", theme.purple),
                _ => ("  ", theme.dim),
            };

            let mut spans = vec![
                Span::styled(
                    if is_cursor || is_selected { "▍" } else { " " },
                    Style::default().bg(bg).fg(theme.accent),
                ),
                Span::styled(
                    format!(
                        "{:>4} {:>4}",
                        line.old_line.map(|n| n.to_string()).unwrap_or_default(),
                        line.new_line.map(|n| n.to_string()).unwrap_or_default(),
                    ),
                    Style::default().bg(bg).fg(theme.dim),
                ),
                Span::styled(marker, Style::default().bg(bg).fg(marker_color)),
                Span::styled(sigil, Style::default().bg(bg).fg(theme.dim)),
            ];

            let mut used = GUTTER;
            match styled.and_then(|s| s.get(index)).filter(|s| !s.is_empty()) {
                Some(segments) => {
                    for segment in segments {
                        let Some(source) = line.text.get(segment.range.clone()) else {
                            continue;
                        };
                        let (text, display_width) = clip(
                            source,
                            width.saturating_sub(used),
                            used.saturating_sub(GUTTER),
                        );
                        if text.is_empty() {
                            break;
                        }
                        used += display_width;

                        let is_plain = !is_cursor && !is_selected;
                        let seg_bg = if segment.is_emphasis && is_plain {
                            strong_bg
                        } else {
                            bg
                        };
                        spans.push(Span::styled(
                            text,
                            Style::default().bg(seg_bg).fg(Color::Rgb(
                                segment.color.0,
                                segment.color.1,
                                segment.color.2,
                            )),
                        ));
                    }
                }
                None => {
                    let (text, display_width) =
                        clip(&line.text, width.saturating_sub(used), used - GUTTER);
                    used += display_width;
                    spans.push(Span::styled(text, Style::default().bg(bg).fg(theme.code)));
                }
            }

            spans.push(Span::styled(
                " ".repeat(width.saturating_sub(used)),
                Style::default().bg(bg),
            ));

            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(rows), inner);
}

/// Floats over the diff so the anchored lines stay visible while typing.
fn draw_composer(frame: &mut Frame, app: &mut App, area: Rect) {
    let theme = app.theme();
    let Some(composer) = app.composer.as_ref() else {
        return;
    };

    let height = 10.min(area.height);
    let rect = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(height),
        width: area.width,
        height,
    };

    let anchor = composer.anchor;
    let span = if anchor.start_line == anchor.end_line {
        format!("{}", anchor.start_line)
    } else {
        format!("{}-{}", anchor.start_line, anchor.end_line)
    };

    let name = composer.path.rsplit('/').next().unwrap_or(&composer.path);
    let title = format!(
        " comment · {name}:{span} · {} ",
        anchor.side.as_api().to_lowercase()
    );

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.orange))
        .title(Span::styled(
            title,
            Style::default()
                .fg(theme.orange)
                .add_modifier(Modifier::BOLD),
        ));

    let inner = block.inner(rect);

    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    if inner.is_empty() {
        return;
    }

    let (cursor_row, cursor_byte) = composer.editor.cursor();
    let lines = composer.editor.lines();
    let first_row = cursor_row.saturating_sub(inner.height.saturating_sub(1) as usize);
    let cursor_column = terminal_width(&lines[cursor_row][..cursor_byte]);
    let first_column = cursor_column.saturating_sub(inner.width.saturating_sub(1) as usize);

    let visible: Vec<Line> = lines
        .iter()
        .skip(first_row)
        .take(inner.height as usize)
        .map(|line| {
            Line::styled(
                clip_window(line, first_column, inner.width as usize),
                Style::default().fg(theme.code),
            )
        })
        .collect();
    frame.render_widget(Paragraph::new(visible), inner);
    frame.set_cursor_position((
        inner.x + cursor_column.saturating_sub(first_column) as u16,
        inner.y + cursor_row.saturating_sub(first_row) as u16,
    ));
}

fn draw_bottom_bar(frame: &mut Frame, app: &App, pending_hint: &str, area: Rect) {
    let theme = app.theme();
    let bar = Style::default().bg(theme.hunk);
    let mode_bg = match app.mode {
        Mode::Normal => theme.accent,
        Mode::Visual => theme.orange,
        Mode::Insert => theme.success,
        Mode::Filter => theme.purple,
    };

    let pane = match app.pane {
        Pane::Files => " files",
        Pane::Diff => " diff",
    };

    let show_match_position = app.mode == Mode::Filter
        || (app.mode == Mode::Normal && app.pane == Pane::Files && app.file_filter.is_some());
    let position = match (show_match_position, app.current_file()) {
        (true, _) => {
            let matches = app.filtered_file_indices();
            let selected = matches
                .iter()
                .position(|&index| index == app.selected_file)
                .map_or(0, |position| position + 1);
            format!("  {selected}/{} matches", matches.len())
        }
        (false, Some(file)) => format!(
            "  {}/{} · {}/{}",
            app.selected_file + 1,
            app.files.len(),
            (app.cursor + 1).min(file.lines.len().max(1)),
            file.lines.len()
        ),
        (false, None) => String::new(),
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
        Span::styled(position, bar.fg(theme.muted)),
    ];

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
            format!("   ▤ {comments}"),
            bar.fg(theme.purple),
        ));
    }

    if !app.drafts.is_empty() {
        spans.push(Span::styled(
            format!("   ✎ {}", app.drafts.len()),
            bar.fg(theme.orange),
        ));
    }

    if !app.status.is_empty() {
        spans.push(Span::styled(
            format!("   {}", app.status),
            bar.fg(if app.status.starts_with("error:") {
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

    let keys: &[(&str, &str)] = match (app.mode, app.pane) {
        (Mode::Filter, _) => &[("↑↓", "select"), ("↵", "apply"), ("esc", "cancel")],
        (Mode::Insert, _) => &[("^s", "save"), ("esc", "cancel")],
        (Mode::Visual, _) => &[("j/k", "extend"), ("c", "comment"), ("esc", "cancel")],
        (Mode::Normal, Pane::Files) => &[
            ("j/k", "move"),
            ("↵", "open"),
            if app.file_filter.is_some() {
                ("/", "edit filter")
            } else {
                ("/", "filter")
            },
        ],
        (Mode::Normal, Pane::Diff) => &[("j/k", "move"), ("c", "comment"), ("⇥", "files")],
    };

    let available = (area.width as usize).saturating_sub(left_width + 2);
    let mut hint_spans = Vec::new();
    let mut hint_width = 0;
    for &(key, label) in keys {
        let pair_width = terminal_width(key) + terminal_width(label) + 3;
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

    if hint_width > 0 {
        let hint_area = Rect {
            x: area.x + area.width.saturating_sub(hint_width as u16),
            width: hint_width as u16,
            ..area
        };
        frame.render_widget(
            Paragraph::new(Line::from(hint_spans)).alignment(Alignment::Right),
            hint_area,
        );
    }
}

fn draw_loading(frame: &mut Frame, app: &App, area: Rect) {
    if area.is_empty() {
        return;
    }

    let theme = app.theme();
    const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];
    let line = Line::from(vec![
        Span::styled(
            SPINNER[app.loading_frame % SPINNER.len()],
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  loading changes", Style::default().fg(theme.dim)),
    ]);
    let y = area.y + area.height.saturating_sub(1) / 2;
    frame.render_widget(
        Paragraph::new(line).alignment(Alignment::Center),
        Rect {
            y,
            height: 1,
            ..area
        },
    );
}

fn draw_empty_pane(frame: &mut Frame, app: &App, area: Rect, message: &str) {
    if area.is_empty() {
        return;
    }

    let y = area.y + area.height.saturating_sub(1) / 2;
    frame.render_widget(
        Paragraph::new(Line::styled(message, Style::default().fg(app.theme().dim)))
            .alignment(Alignment::Center),
        Rect {
            y,
            height: 1,
            ..area
        },
    );
}

/// Expand tabs at real tab stops and clip by terminal cells, not scalar count.
fn clip(text: &str, width: usize, column: usize) -> (Cow<'_, str>, usize) {
    if width == 0 {
        return (Cow::Borrowed(""), 0);
    }

    let display_width = UnicodeWidthStr::width(text);
    if !text.contains('\t') && display_width <= width {
        return (Cow::Borrowed(text), display_width);
    }

    let mut rendered = String::with_capacity(text.len().min(width));
    let mut used = 0;
    for character in text.chars() {
        if character == '\t' {
            let tab_width = 4 - ((column + used) % 4);
            if used + tab_width > width {
                break;
            }
            rendered.extend(std::iter::repeat_n(' ', tab_width));
            used += tab_width;
            continue;
        }

        let character_width = UnicodeWidthChar::width(character).unwrap_or(0);
        if used + character_width > width {
            break;
        }
        rendered.push(character);
        used += character_width;
    }

    (Cow::Owned(rendered), used)
}

fn terminal_width(text: &str) -> usize {
    text.chars().fold(0, |column, character| {
        column
            + if character == '\t' {
                4 - (column % 4)
            } else {
                UnicodeWidthChar::width(character).unwrap_or(0)
            }
    })
}

fn clip_window(text: &str, start: usize, width: usize) -> String {
    let mut rendered = String::with_capacity(text.len().min(width));
    let end = start.saturating_add(width);
    let mut column = 0;

    for character in text.chars() {
        let character_width = if character == '\t' {
            4 - (column % 4)
        } else {
            UnicodeWidthChar::width(character).unwrap_or(0)
        };
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

fn truncate_right(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }

    let head: String = text.chars().take(width.saturating_sub(1)).collect();
    format!("{head}…")
}

/// Splits `a/b/c.rs` into a dimmed `a/b/` and a bright `c.rs`, eliding from
/// the left when it will not fit.
fn split_path(path: &str, width: usize) -> (String, String) {
    let name = path.rsplit('/').next().unwrap_or(path).to_string();

    if name.chars().count() >= width {
        let tail: String = name
            .chars()
            .skip(name.chars().count() + 1 - width)
            .collect();
        return (String::new(), format!("…{tail}"));
    }

    let dir_width = width - name.chars().count();
    let dir = path.strip_suffix(&name).unwrap_or("");

    if dir.chars().count() <= dir_width {
        return (dir.to_string(), name);
    }

    let tail: String = dir
        .chars()
        .skip(dir.chars().count() + 1 - dir_width)
        .collect();
    (format!("…{tail}"), name)
}

pub fn diff_viewport_height(area: Rect) -> usize {
    // Header, pane title rule, and the compact bottom bar each consume a row.
    area.height.saturating_sub(3) as usize
}
