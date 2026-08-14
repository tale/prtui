use crate::app::draft::{Draft, Side};
use crate::app::mode::Mode;
use crate::app::{App, Pane};
use crate::model::LineKind;
use edtui::{EditorTheme, EditorView};
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, Clear, Paragraph};
use ratatui::Frame;

/// The page itself stays on the terminal's own background; only diff state
/// paints a tint.
const BG: Color = Color::Reset;
const BG_ADD: Color = Color::Rgb(29, 51, 41);
const BG_DEL: Color = Color::Rgb(58, 34, 38);
const BG_ADD_STRONG: Color = Color::Rgb(46, 92, 68);
const BG_DEL_STRONG: Color = Color::Rgb(105, 51, 58);
const BG_CURSOR: Color = Color::Rgb(58, 64, 79);
const BG_SELECT: Color = Color::Rgb(67, 76, 94);
const BG_HUNK: Color = Color::Rgb(38, 43, 54);

/// Text drawn on top of an accent pill, which is opaque regardless of theme.
const INK: Color = Color::Rgb(28, 32, 40);
const FG_DIM: Color = Color::Rgb(76, 86, 106);
const FG_MUTED: Color = Color::Rgb(129, 161, 193);
const FG_CODE: Color = Color::Rgb(216, 222, 233);
const ACCENT: Color = Color::Rgb(136, 192, 208);
const PURPLE: Color = Color::Rgb(180, 142, 173);
const ORANGE: Color = Color::Rgb(208, 135, 112);

const GUTTER: usize = 11;

pub fn draw(frame: &mut Frame, app: &mut App, pending_hint: &str) {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    draw_header(frame, app, rows[0]);

    if app.is_files_visible {
        let width = files_width(rows[1].width);
        let cols = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Length(width), Constraint::Min(20)])
            .split(rows[1]);

        draw_files(frame, app, cols[0]);
        draw_diff(frame, app, cols[1]);
    } else {
        draw_diff(frame, app, rows[1]);
    }

    draw_status(frame, app, pending_hint, rows[2]);
    draw_help(frame, app, rows[3]);
    draw_composer(frame, app, rows[1]);
}

/// Roughly a quarter of the terminal, clamped so the tree neither crowds the
/// diff on a narrow window nor sprawls on a wide one.
fn files_width(total: u16) -> u16 {
    (total / 4).clamp(22, 34).min(total.saturating_sub(20))
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let spans = match &app.pr {
        None => vec![Span::styled(" loading…", Style::default().fg(FG_DIM))],
        Some(pr) => {
            let (label, color) = match pr.state.as_str() {
                "MERGED" => (" merged ", Color::Rgb(180, 142, 173)),
                "CLOSED" => (" closed ", Color::Rgb(191, 97, 106)),
                _ if pr.is_draft => (" draft ", FG_DIM),
                _ => (" open ", Color::Rgb(163, 190, 140)),
            };

            vec![
                Span::styled(
                    label,
                    Style::default().bg(color).fg(INK).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" #{} ", pr.number),
                    Style::default().fg(Color::Rgb(235, 203, 139)).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    truncate_right(&pr.title, area.width as usize / 2),
                    Style::default().fg(Color::Rgb(236, 239, 244)).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  {} → {}", pr.head_ref, pr.base_ref),
                    Style::default().fg(FG_MUTED),
                ),
                Span::styled(format!("  @{}", pr.author), Style::default().fg(FG_DIM)),
            ]
        }
    };

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_files(frame: &mut Frame, app: &App, area: Rect) {
    let is_focused = app.pane == Pane::Files;
    let block = Block::default()
        .borders(Borders::RIGHT)
        .border_style(Style::default().fg(if is_focused { ACCENT } else { FG_DIM }));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let height = inner.height as usize;
    let width = inner.width as usize;
    let start = app
        .selected_file
        .saturating_sub(height / 2)
        .min(app.files.len().saturating_sub(height));

    let rows: Vec<Line> = app
        .files
        .iter()
        .enumerate()
        .skip(start)
        .take(height)
        .map(|(index, file)| {
            let is_selected = index == app.selected_file;
            let unresolved = app
                .threads_by_path
                .get(&file.path)
                .map(|list| list.iter().filter(|t| !t.is_resolved).count())
                .unwrap_or(0);

            let marker = if unresolved > 0 { format!(" {unresolved}◆") } else { "  ".into() };
            let adds = format!("+{}", file.additions);
            let dels = format!("-{}", file.deletions);

            let counts_width = adds.len().max(5) + dels.len().max(5) + 1;
            let name_width = width.saturating_sub(counts_width + marker.chars().count() + 2);

            let base = if is_selected {
                Style::default().bg(BG_CURSOR).add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };

            let (dir, name) = split_path(&file.path, name_width);
            let status_color = match file.status.as_str() {
                "added" => Color::Rgb(163, 190, 140),
                "removed" => Color::Rgb(191, 97, 106),
                "renamed" => Color::Rgb(235, 203, 139),
                _ => FG_MUTED,
            };

            let pad = name_width.saturating_sub(dir.chars().count() + name.chars().count());

            Line::from(vec![
                Span::styled(if is_selected { " ▍" } else { "  " }, base.fg(ACCENT)),
                Span::styled(dir, base.fg(FG_DIM)),
                Span::styled(name, base.fg(status_color)),
                Span::styled(" ".repeat(pad), base),
                Span::styled(marker, base.fg(Color::Rgb(180, 142, 173))),
                Span::styled(format!("{adds:>5}"), base.fg(Color::Rgb(163, 190, 140))),
                Span::styled(format!(" {dels:>5}"), base.fg(Color::Rgb(191, 97, 106))),
            ])
        })
        .collect();

    frame.render_widget(Paragraph::new(rows), inner);
}

fn draw_diff(frame: &mut Frame, app: &App, area: Rect) {
    let Some(file) = app.current_file() else { return };

    let height = area.height as usize;
    let width = area.width as usize;
    let threads = app.threads_by_path.get(&file.path);
    let styled = app.highlighted();
    let is_focused = app.pane == Pane::Diff;

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
                let bg = if is_selected { BG_SELECT } else { BG_HUNK };

                return Line::from(Span::styled(
                    text,
                    Style::default().bg(bg).fg(FG_MUTED).add_modifier(Modifier::ITALIC),
                ));
            }

            let (base_bg, strong_bg, sigil) = match line.kind {
                LineKind::Added => (BG_ADD, BG_ADD_STRONG, "+"),
                LineKind::Removed => (BG_DEL, BG_DEL_STRONG, "-"),
                _ => (BG, BG, " "),
            };

            // Selected rows keep their add/remove identity and are lifted instead
            // of flattened; the left bar is what makes the span read as contiguous.
            let bg = match (is_selected, is_cursor) {
                (true, _) => lift(base_bg, 34),
                (false, true) => blend(base_bg),
                _ => base_bg,
            };

            let has_thread = line.new_line.is_some_and(|n| {
                threads.is_some_and(|list| {
                    list.iter().any(|t| t.line == Some(n) && !t.is_resolved)
                })
            });

            let has_draft = drafts.iter().any(|d| match d.side {
                Side::Right => line.new_line.is_some_and(|n| d.covers(&file.path, n, Side::Right)),
                Side::Left => line.old_line.is_some_and(|n| d.covers(&file.path, n, Side::Left)),
            });

            let (marker, marker_color) = match (has_draft, has_thread) {
                (true, _) => (" ✎", ORANGE),
                (false, true) => (" ◆", PURPLE),
                _ => ("  ", FG_DIM),
            };

            let mut spans = vec![
                Span::styled(
                    if is_cursor || is_selected { "▍" } else { " " },
                    Style::default().bg(bg).fg(ACCENT),
                ),
                Span::styled(
                    format!(
                        "{:>4} {:>4}",
                        line.old_line.map(|n| n.to_string()).unwrap_or_default(),
                        line.new_line.map(|n| n.to_string()).unwrap_or_default(),
                    ),
                    Style::default().bg(bg).fg(FG_DIM),
                ),
                Span::styled(marker, Style::default().bg(bg).fg(marker_color)),
                Span::styled(sigil, Style::default().bg(bg).fg(FG_DIM)),
            ];

            let mut used = GUTTER;
            match styled.and_then(|s| s.get(index)).filter(|s| !s.is_empty()) {
                Some(segments) => {
                    for segment in segments {
                        let text = clip(&segment.text, width.saturating_sub(used));
                        if text.is_empty() {
                            break;
                        }
                        used += text.chars().count();

                        let is_plain = !is_cursor && !is_selected;
                        let seg_bg = if segment.is_emphasis && is_plain { strong_bg } else { bg };
                        spans.push(Span::styled(
                            text,
                            Style::default()
                                .bg(seg_bg)
                                .fg(Color::Rgb(segment.color.0, segment.color.1, segment.color.2)),
                        ));
                    }
                }
                None => {
                    let text = clip(&line.text, width.saturating_sub(used));
                    used += text.chars().count();
                    spans.push(Span::styled(
                        text,
                        Style::default().bg(bg).fg(Color::Rgb(216, 222, 233)),
                    ));
                }
            }

            spans.push(Span::styled(
                " ".repeat(width.saturating_sub(used)),
                Style::default().bg(bg),
            ));

            Line::from(spans)
        })
        .collect();

    frame.render_widget(Paragraph::new(rows), area);
}

/// Floats over the diff so the anchored lines stay visible while typing.
fn draw_composer(frame: &mut Frame, app: &mut App, area: Rect) {
    let Some(composer) = app.composer.as_mut() else { return };

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
    let title = format!(" comment · {name}:{span} · {} ", anchor.side.as_api().to_lowercase());

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(ORANGE))
        .title(Span::styled(title, Style::default().fg(ORANGE).add_modifier(Modifier::BOLD)));

    let inner = block.inner(rect);

    frame.render_widget(Clear, rect);
    frame.render_widget(block, rect);

    let theme = EditorTheme::default()
        .base(Style::default().fg(FG_CODE))
        .cursor_style(Style::default().bg(ACCENT).fg(INK))
        .selection_style(Style::default().bg(BG_SELECT))
        .hide_status_line();

    frame.render_widget(EditorView::new(&mut composer.editor).theme(theme), inner);
}

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let keys: &[(&str, &str)] = match (app.mode, app.pane) {
        (Mode::Insert, _) => &[
            ("^s", "save draft"),
            ("^c", "cancel"),
            ("esc", "editor normal"),
        ],
        (Mode::Visual, _) => &[
            ("j/k", "extend"),
            ("c", "comment selection"),
            ("v", "exit visual"),
            ("esc", "cancel"),
        ],
        (Mode::Normal, Pane::Files) => &[
            ("j/k", "file"),
            ("⇥", "diff"),
            ("f", "hide tree"),
            ("gg/G", "top/end"),
            ("q", "quit"),
        ],
        (Mode::Normal, Pane::Diff) => &[
            ("j/k", "line"),
            ("^d/^u", "half page"),
            ("v", "visual"),
            ("c", "comment"),
            ("[/]", "file"),
            ("⇥", "tree"),
            ("q", "quit"),
        ],
    };

    let mut spans = Vec::new();
    for (key, label) in keys {
        spans.push(Span::styled(
            format!(" {key}"),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::styled(format!(" {label}"), Style::default().fg(FG_DIM)));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_status(frame: &mut Frame, app: &App, pending_hint: &str, area: Rect) {
    let mode_bg = match app.mode {
        Mode::Normal => ACCENT,
        Mode::Visual => ORANGE,
        Mode::Insert => Color::Rgb(163, 190, 140),
    };

    let pane = match app.pane {
        Pane::Files => " files ",
        Pane::Diff => " diff ",
    };

    let position = match app.current_file() {
        Some(file) => format!(
            "  {}/{}   ln {}/{}",
            app.selected_file + 1,
            app.files.len(),
            (app.cursor + 1).min(file.lines.len().max(1)),
            file.lines.len()
        ),
        None => String::new(),
    };

    let mut spans = vec![
        Span::styled(
            app.mode.label(),
            Style::default().bg(mode_bg).fg(INK).add_modifier(Modifier::BOLD),
        ),
        Span::styled(pane, Style::default().fg(FG_DIM)),
        Span::styled(position, Style::default().fg(FG_MUTED)),
    ];

    if let Some(selection) = app.selection {
        spans.push(Span::styled(
            format!("   {} lines", selection.row_count()),
            Style::default().fg(ORANGE),
        ));
    }

    if !pending_hint.is_empty() {
        spans.push(Span::styled(
            format!("   {pending_hint}"),
            Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
        ));
    }

    if !app.drafts.is_empty() {
        spans.push(Span::styled(
            format!("   ✎ {}", app.drafts.len()),
            Style::default().fg(ORANGE),
        ));
    }

    if let Some(ms) = app.load_ms {
        spans.push(Span::styled(
            format!("   {ms}ms"),
            Style::default().fg(Color::Rgb(163, 190, 140)),
        ));
    }

    spans.push(Span::styled(format!("   {}", app.status), Style::default().fg(FG_DIM)));

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Lightens a diff tint so the cursor row reads as selected without losing
/// its add/remove identity.
fn blend(color: Color) -> Color {
    lift(color, 24)
}

fn lift(color: Color, amount: u8) -> Color {
    let Color::Rgb(r, g, b) = color else {
        return match amount {
            0..=28 => BG_CURSOR,
            _ => BG_SELECT,
        };
    };

    Color::Rgb(
        r.saturating_add(amount),
        g.saturating_add(amount),
        b.saturating_add(amount),
    )
}

fn clip(text: &str, width: usize) -> String {
    if width == 0 {
        return String::new();
    }

    let expanded = text.replace('\t', "    ");
    if expanded.chars().count() <= width {
        return expanded;
    }

    expanded.chars().take(width).collect()
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
        let tail: String = name.chars().skip(name.chars().count() + 1 - width).collect();
        return (String::new(), format!("…{tail}"));
    }

    let dir_width = width - name.chars().count();
    let dir = path.strip_suffix(&name).unwrap_or("");

    if dir.chars().count() <= dir_width {
        return (dir.to_string(), name);
    }

    let tail: String = dir.chars().skip(dir.chars().count() + 1 - dir_width).collect();
    (format!("…{tail}"), name)
}

pub fn diff_viewport_height(area: Rect) -> usize {
    area.height.saturating_sub(2) as usize
}
