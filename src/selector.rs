use crate::terminal;
use anyhow::{Context, Result};
use prtui::gh::{PullRequestList, PullRequestTarget, ReviewStatus};
use prtui::renderer::{Theme, ThemeMode};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, HighlightSpacing, Paragraph, Row, Table,
    TableState,
};
use termina::escape::csi::{
    Csi, Mode as CsiMode, ThemeMode as TerminalThemeMode,
};
use termina::event::{KeyCode, KeyEventKind, Modifiers};
use termina::{Event, EventStream};

pub async fn select(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    pull_requests: PullRequestList,
    theme: &mut Theme,
    follow_terminal: bool,
) -> Result<Option<PullRequestTarget>> {
    let mut selected = (!pull_requests.is_empty()).then_some(0);

    loop {
        terminal::render(terminal, |frame| {
            draw(frame, &pull_requests, selected, *theme);
        })
        .context("drawing pull request selector")?;

        let event = crate::next_event(events)
            .await
            .context("terminal event stream closed")?
            .context("reading terminal event")?;

        match event {
            Event::Key(key)
                if matches!(
                    key.kind,
                    KeyEventKind::Press | KeyEventKind::Repeat
                ) =>
            {
                match (key.code, key.modifiers) {
                    (KeyCode::Char('c'), Modifiers::CONTROL)
                    | (KeyCode::Escape | KeyCode::Char('q'), Modifiers::NONE) =>
                    {
                        return Ok(None);
                    }
                    (KeyCode::Enter, Modifiers::NONE) => {
                        if let Some(selected) = selected {
                            return Ok(pull_requests.select(selected));
                        }
                    }
                    (KeyCode::Up | KeyCode::Char('k'), Modifiers::NONE) => {
                        selected =
                            selected.map(|index| index.saturating_sub(1));
                    }
                    (KeyCode::Down | KeyCode::Char('j'), Modifiers::NONE) => {
                        selected = selected.map(|index| {
                            index
                                .saturating_add(1)
                                .min(pull_requests.len().saturating_sub(1))
                        });
                    }
                    (KeyCode::Char('g'), Modifiers::NONE) => {
                        selected = (!pull_requests.is_empty()).then_some(0);
                    }
                    (
                        KeyCode::Char('G'),
                        Modifiers::NONE | Modifiers::SHIFT,
                    ) => {
                        selected = (!pull_requests.is_empty())
                            .then_some(pull_requests.len().saturating_sub(1));
                    }
                    _ => {}
                }
            }
            Event::Csi(Csi::Mode(CsiMode::ReportTheme(terminal_mode)))
                if follow_terminal =>
            {
                let mode = match terminal_mode {
                    TerminalThemeMode::Dark => ThemeMode::Dark,
                    TerminalThemeMode::Light => ThemeMode::Light,
                };
                *theme = Theme::for_mode(mode);
            }
            _ => {}
        }
    }
}

fn draw(
    frame: &mut Frame,
    pull_requests: &PullRequestList,
    selected: Option<usize>,
    theme: Theme,
) {
    let area = frame.area();
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::styled(
            format!(" Open pull requests · {} ", pull_requests.len()),
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(Line::styled(
            " j/k select · enter open · esc cancel ",
            Style::default().fg(theme.dim),
        ));
    let inner = block.inner(area);

    let mut headers = vec![Cell::from("REVIEW")];
    let mut widths = vec![Constraint::Length(18)];
    if pull_requests.shows_repositories() {
        headers.push(Cell::from("REPOSITORY"));
        widths.push(Constraint::Length(28));
    }
    headers.extend([Cell::from("PR"), Cell::from("TITLE")]);
    widths.extend([Constraint::Length(8), Constraint::Fill(1)]);

    let header = Row::new(headers)
        .style(
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);
    let rows = (0..pull_requests.len()).filter_map(|index| {
        let choice = pull_requests.row(index)?;
        let mut cells = vec![review_cell(choice.review_status, theme)];
        if pull_requests.shows_repositories() {
            let repository = choice
                .repository
                .map_or_else(String::new, prtui::gh::Repo::slug);
            cells.push(
                Cell::from(repository).style(Style::default().fg(theme.muted)),
            );
        }
        cells.extend([
            Cell::from(format!("#{}", choice.number))
                .style(Style::default().fg(theme.warning)),
            Cell::from(choice.title).style(Style::default().fg(theme.code)),
        ]);

        Some(Row::new(cells))
    });

    let table = Table::new(rows, widths)
        .header(header)
        .block(block)
        .column_spacing(2)
        .highlight_symbol(" ▍")
        .highlight_spacing(HighlightSpacing::Always)
        .row_highlight_style(
            Style::default()
                .bg(theme.cursor)
                .add_modifier(Modifier::BOLD),
        );

    let mut table_state = TableState::default().with_selected(selected);
    frame.render_stateful_widget(table, area, &mut table_state);

    if pull_requests.is_empty() && !inner.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::styled(
                "no open pull requests",
                Style::default().fg(theme.dim),
            ))
            .alignment(Alignment::Center),
            Rect {
                y: inner.y + inner.height.saturating_sub(1) / 2,
                height: 1,
                ..inner
            },
        );
    }
}

fn review_cell(status: &ReviewStatus, theme: Theme) -> Cell<'static> {
    let (label, color) = match status {
        ReviewStatus::Draft => ("DRAFT", theme.dim),
        ReviewStatus::ChangesRequested => ("CHANGES REQUESTED", theme.danger),
        ReviewStatus::ReviewRequired => ("REVIEW REQUIRED", theme.warning),
        ReviewStatus::Approved => ("APPROVED", theme.success),
        ReviewStatus::NoDecision => ("NO DECISION", theme.muted),
    };

    Cell::from(Line::from(Span::styled(
        label,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use prtui::gh::{LocatedPullRequest, PullRequest, Repo};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn pull(
        number: u32,
        title: &str,
        review_status: ReviewStatus,
    ) -> PullRequest {
        PullRequest {
            number,
            title: title.into(),
            review_status,
        }
    }

    fn located(
        number: u32,
        title: &str,
        review_status: ReviewStatus,
    ) -> LocatedPullRequest {
        LocatedPullRequest {
            repo: Repo::parse("owner/repo").unwrap(),
            pull: pull(number, title, review_status),
        }
    }

    fn render(pull_requests: &PullRequestList) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 20)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, pull_requests, Some(0), Theme::dark());
            })
            .unwrap();

        terminal.backend().to_string()
    }

    #[test]
    fn dashboard_uses_the_full_frame_and_shows_every_review_status() {
        let pull_requests = PullRequestList::User {
            pulls: vec![
                located(1, "Draft", ReviewStatus::Draft),
                located(2, "Needs work", ReviewStatus::ChangesRequested),
                located(3, "Needs review", ReviewStatus::ReviewRequired),
                located(4, "Ready", ReviewStatus::Approved),
                located(5, "Pending", ReviewStatus::NoDecision),
            ],
        };
        let rendered = render(&pull_requests);

        assert!(rendered.contains("Open pull requests · 5"));
        assert!(rendered.contains("REVIEW"));
        assert!(rendered.contains("REPOSITORY"));
        assert!(rendered.contains("DRAFT"));
        assert!(rendered.contains("CHANGES REQUESTED"));
        assert!(rendered.contains("REVIEW REQUIRED"));
        assert!(rendered.contains("APPROVED"));
        assert!(rendered.contains("NO DECISION"));
        assert!(rendered.contains("owner/repo"));
    }

    #[test]
    fn repository_dashboard_omits_the_redundant_repository_column() {
        let pull_requests = PullRequestList::Repository {
            repo: Repo::parse("owner/repo").unwrap(),
            pulls: vec![pull(42, "Local change", ReviewStatus::Approved)],
        };
        let rendered = render(&pull_requests);

        assert!(!rendered.contains("REPOSITORY"));
        assert!(rendered.contains("Local change"));
    }

    #[test]
    fn an_empty_dashboard_cannot_select_a_pull_request() {
        let pull_requests = PullRequestList::Repository {
            repo: Repo::parse("owner/repo").unwrap(),
            pulls: Vec::new(),
        };
        assert!(pull_requests.select(0).is_none());
    }
}
