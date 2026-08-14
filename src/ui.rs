use crate::app::{App, Pane};
use crate::model::LineKind;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

/// The page itself stays on the terminal's own background; only diff state
/// paints a tint.
const BG: Color = Color::Reset;
const BG_ADD: Color = Color::Rgb(29, 51, 41);
const BG_DEL: Color = Color::Rgb(58, 34, 38);
const BG_ADD_STRONG: Color = Color::Rgb(46, 92, 68);
const BG_DEL_STRONG: Color = Color::Rgb(105, 51, 58);
const BG_CURSOR: Color = Color::Rgb(58, 64, 79);
const BG_HUNK: Color = Color::Rgb(38, 43, 54);

/// Text drawn on top of an accent pill, which is opaque regardless of theme.
const INK: Color = Color::Rgb(28, 32, 40);
const FG_DIM: Color = Color::Rgb(76, 86, 106);
const FG_MUTED: Color = Color::Rgb(129, 161, 193);
const ACCENT: Color = Color::Rgb(136, 192, 208);

const GUTTER: usize = 11;

pub fn draw(frame: &mut Frame, app: &App) {
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

    draw_status(frame, app, rows[2]);
    draw_help(frame, app, rows[3]);
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

            if line.kind == LineKind::Hunk {
                let text = format!("{:<width$}", line.text, width = width);
                return Line::from(Span::styled(
                    text,
                    Style::default().bg(BG_HUNK).fg(FG_MUTED).add_modifier(Modifier::ITALIC),
                ));
            }

            let (bg, strong_bg, sigil) = match line.kind {
                LineKind::Added => (BG_ADD, BG_ADD_STRONG, "+"),
                LineKind::Removed => (BG_DEL, BG_DEL_STRONG, "-"),
                _ => (BG, BG, " "),
            };

            let bg = if is_cursor { blend(bg) } else { bg };

            let has_thread = line.new_line.is_some_and(|n| {
                threads.is_some_and(|list| {
                    list.iter().any(|t| t.line == Some(n) && !t.is_resolved)
                })
            });

            let mut spans = vec![
                Span::styled(
                    if is_cursor { "▍" } else { " " },
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
                Span::styled(
                    if has_thread { " ◆" } else { "  " },
                    Style::default().bg(bg).fg(Color::Rgb(180, 142, 173)),
                ),
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

                        let seg_bg = if segment.is_emphasis && !is_cursor { strong_bg } else { bg };
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

fn draw_help(frame: &mut Frame, app: &App, area: Rect) {
    let keys: &[(&str, &str)] = match app.pane {
        Pane::Files => &[
            ("j/k", "file"),
            ("⇥", "diff"),
            ("f", "hide tree"),
            ("g/G", "top/end"),
            ("q", "quit"),
        ],
        Pane::Diff => &[
            ("j/k", "line"),
            ("d/u", "half page"),
            ("[/]", "file"),
            ("⇥", "tree"),
            ("f", "toggle tree"),
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

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let (mode, mode_bg) = match app.pane {
        Pane::Files => (" FILES ", ACCENT),
        Pane::Diff => (" DIFF ", Color::Rgb(163, 190, 140)),
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
            mode,
            Style::default().bg(mode_bg).fg(INK).add_modifier(Modifier::BOLD),
        ),
        Span::styled(position, Style::default().fg(FG_MUTED)),
    ];

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
    let Color::Rgb(r, g, b) = color else { return BG_CURSOR };

    Color::Rgb(
        r.saturating_add(22),
        g.saturating_add(24),
        b.saturating_add(26),
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
