//! The landing page: every open pull request, and a summary of the one under
//! the cursor.
//!
//! Keys reach it through the same keymap the review surface uses, so a count, a
//! half-page scroll and a search mean here what they mean there.

use crate::summary;
use crate::terminal;
use anyhow::{Context, Result};
use prtui::app::action::Action;
use prtui::app::keymap::{Keymap, Resolution};
use prtui::app::mode::Mode;
use prtui::app::search::Query;
use prtui::gh::{
    self, PullRequestList, PullRequestTarget, ReviewStatus, Summary,
};
use prtui::renderer::{Theme, ThemeMode};
use prtui::ui::{self, SPINNER};
use prtui::vim::Cursor;
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row,
    Table, TableState,
};
use std::time::Duration;
use termina::escape::csi::{
    Csi, Mode as CsiMode, ThemeMode as TerminalThemeMode,
};
use termina::event::{KeyCode, KeyEvent, KeyEventKind, Modifiers};
use termina::{Event, EventStream};
use tokio::sync::mpsc;

/// The selector's keymap, in the same notation as the review surface's.
///
/// `K` is bound where an editor puts hover: the summary of the thing under the
/// cursor, without leaving the list to get it.
const KEYS: &[(&str, &str, &str)] = &[
    ("no", "j", "move-down"),
    ("no", "k", "move-up"),
    ("no", "<Down>", "move-down"),
    ("no", "<Up>", "move-up"),
    ("no", "<C-d>", "half-page-down"),
    ("no", "<C-u>", "half-page-up"),
    ("no", "gg", "goto-first-line"),
    ("no", "G", "goto-last-line"),
    ("n", "/", "find"),
    ("n", "K", "overview"),
    ("no", "<CR>", "activate"),
    ("n", "q", "quit"),
    ("n", "<Esc>", "escape"),
    ("o", "K", "close-panel"),
    ("o", "q", "close-panel"),
    ("o", "<Esc>", "close-panel"),
    ("f", "<CR>", "accept-filter"),
    ("f", "<Esc>", "cancel-filter"),
    ("f", "<Down>", "move-down"),
    ("f", "<Up>", "move-up"),
    ("nof", "<C-c>", "quit"),
];

/// Rows of chrome the table spends before its first pull request: the header
/// and the blank row under it.
const HEADER_ROWS: u16 = 2;

/// Widest the summary panel gets before it stops using the whole terminal.
const PANEL_WIDTH: u16 = 62;

enum Message {
    Listed(Result<PullRequestList>),
    Summarized(PullRequestTarget, Result<Summary>),
}

/// What the selector has to paint. The listing is read on its own task, and a
/// failure is shown in the frame rather than tearing the session down.
enum Listing {
    Loading,
    Ready(PullRequestList),
    Failed(String),
}

impl Listing {
    const fn is_loading(&self) -> bool {
        matches!(self, Self::Loading)
    }

    const fn rows(&self) -> Option<&PullRequestList> {
        match self {
            Self::Ready(pull_requests) => Some(pull_requests),
            _ => None,
        }
    }

    fn len(&self) -> usize {
        self.rows().map_or(0, PullRequestList::len)
    }
}

/// The summary panel, pinned to the pull request it was opened on the way an
/// editor's hover is pinned to the symbol under the cursor.
struct Panel {
    target: PullRequestTarget,
    state: PanelState,
    scroll: usize,
}

enum PanelState {
    Loading,
    Ready(Box<Summary>),
    Failed(String),
}

pub struct Selector {
    listing: Listing,
    cursor: Cursor,
    mode: Mode,
    keymap: Keymap,
    /// The `/` line, which narrows the list on every keystroke rather than on
    /// the one that ends it.
    filter: String,
    /// The rows the filter leaves, as indices into the listing.
    visible: Vec<usize>,
    /// The row `/` was opened on, which cancelling puts back.
    snapshot: Option<usize>,
    panel: Option<Panel>,
    status: String,
    loading_frame: usize,
    is_done: bool,
    chosen: Option<PullRequestTarget>,
}

impl Selector {
    fn new() -> Self {
        Self {
            listing: Listing::Loading,
            cursor: Cursor::default(),
            mode: Mode::Normal,
            keymap: Keymap::from_table(KEYS),
            filter: String::new(),
            visible: Vec::new(),
            snapshot: None,
            panel: None,
            status: String::new(),
            loading_frame: 0,
            is_done: false,
            chosen: None,
        }
    }

    const fn is_waiting(&self) -> bool {
        self.listing.is_loading()
            || matches!(
                self.panel,
                Some(Panel {
                    state: PanelState::Loading,
                    ..
                })
            )
    }

    /// The row the cursor is on, when the filter leaves one.
    fn selected(&self) -> Option<usize> {
        (!self.visible.is_empty()).then_some(self.cursor.index)
    }

    fn target(&self) -> Option<PullRequestTarget> {
        let row = *self.visible.get(self.cursor.index)?;

        self.listing.rows()?.target(row)
    }

    /// Reruns the filter, keeping the cursor on the pull request it was on
    /// when that one survives the narrowing.
    fn sync_visible(&mut self, viewport: usize) {
        let current = self.visible.get(self.cursor.index).copied();
        let Some(pull_requests) = self.listing.rows() else {
            self.visible.clear();
            return;
        };

        self.visible = match Query::new(&self.filter) {
            None => (0..pull_requests.len()).collect(),
            Some(query) => (0..pull_requests.len())
                .filter(|index| {
                    pull_requests
                        .row(*index)
                        .is_some_and(|row| query.is_match(&row_text(&row)))
                })
                .collect(),
        };

        let landing = current
            .and_then(|row| self.visible.iter().position(|shown| *shown == row))
            .unwrap_or(0);
        self.cursor.jump(landing, self.visible.len(), viewport);
    }

    /// Puts the cursor back on one pull request of the listing, by the index
    /// the listing itself uses.
    fn land_on(&mut self, row: usize, viewport: usize) {
        let landing = self
            .visible
            .iter()
            .position(|shown| *shown == row)
            .unwrap_or(0);

        self.cursor.jump(landing, self.visible.len(), viewport);
    }

    fn receive(&mut self, message: Message, viewport: usize) {
        match message {
            Message::Listed(Ok(pull_requests)) => {
                self.listing = Listing::Ready(pull_requests);
                self.sync_visible(viewport);
            }
            Message::Listed(Err(err)) => {
                self.listing = Listing::Failed(format!("error: {err}"));
            }
            Message::Summarized(target, summarized) => {
                self.set_summary(&target, summarized);
            }
        }
    }

    /// A summary that arrives after the panel moved on belongs to nothing on
    /// screen, so it is dropped rather than painted over the panel that
    /// replaced it.
    fn set_summary(
        &mut self,
        target: &PullRequestTarget,
        summarized: Result<Summary>,
    ) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };

        if panel.target.number != target.number
            || panel.target.repo.slug() != target.repo.slug()
        {
            return;
        }

        panel.state = match summarized {
            Ok(summary) => PanelState::Ready(Box::new(summary)),
            Err(err) => PanelState::Failed(format!("error: {err}")),
        };
    }

    /// One keystroke, resolved against the keymap and applied. Answers with the
    /// pull request to summarize, which the caller fetches.
    fn press(
        &mut self,
        key: KeyEvent,
        viewport: usize,
    ) -> Option<PullRequestTarget> {
        match self.keymap.resolve(self.mode, key) {
            Resolution::Action(action) => self.apply(&action, viewport),
            Resolution::Pending => None,
            Resolution::Unbound => {
                self.type_key(key, viewport);
                None
            }
        }
    }

    fn apply(
        &mut self,
        action: &Action,
        viewport: usize,
    ) -> Option<PullRequestTarget> {
        match action {
            Action::Move(motion) => {
                match self.panel.as_mut() {
                    Some(panel) => {
                        let mut cursor = Cursor::at(panel.scroll);
                        cursor.apply(*motion, panel_len(panel), viewport);
                        panel.scroll = cursor.index;
                    }
                    None => {
                        self.cursor.apply(
                            *motion,
                            self.visible.len(),
                            viewport,
                        );
                    }
                }
                self.status.clear();
            }
            Action::Activate => {
                self.chosen = self.target();
                self.is_done = self.chosen.is_some();
            }
            Action::Quit => self.is_done = true,
            Action::Escape => {
                if self.filter.is_empty() {
                    self.is_done = true;
                    return None;
                }

                self.filter.clear();
                self.sync_visible(viewport);
            }
            Action::OpenOverview => return self.open_panel(),
            Action::CloseOverlay => self.close_panel(),
            // The line starts empty each time: the list it narrows is right
            // there, so there is nothing to recall.
            Action::StartFind => {
                self.snapshot = self.visible.get(self.cursor.index).copied();
                self.filter.clear();
                self.sync_visible(viewport);
                self.set_mode(Mode::Filter);
            }
            Action::AcceptFileFilter => {
                self.snapshot = None;
                self.set_mode(Mode::Normal);
            }
            Action::CancelFileFilter => {
                self.filter.clear();
                self.sync_visible(viewport);

                if let Some(row) = self.snapshot.take() {
                    self.land_on(row, viewport);
                }
                self.set_mode(Mode::Normal);
            }
            _ => {}
        }

        None
    }

    /// Typing only ever reaches the `/` line: nothing else here takes text.
    fn type_key(&mut self, key: KeyEvent, viewport: usize) {
        if self.mode != Mode::Filter {
            return;
        }

        match key.code {
            KeyCode::Char(character)
                if !key.modifiers.contains(Modifiers::CONTROL) =>
            {
                self.filter.push(character);
            }
            KeyCode::Backspace => {
                self.filter.pop();
            }
            _ => return,
        }

        self.sync_visible(viewport);
    }

    fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        self.keymap.clear();
    }

    fn open_panel(&mut self) -> Option<PullRequestTarget> {
        let (target, asked) = (self.target()?, self.target()?);

        self.panel = Some(Panel {
            target,
            state: PanelState::Loading,
            scroll: 0,
        });
        self.set_mode(Mode::Overview);

        Some(asked)
    }

    fn close_panel(&mut self) {
        self.panel = None;
        self.set_mode(Mode::Normal);
    }
}

const fn panel_len(panel: &Panel) -> usize {
    match &panel.state {
        PanelState::Ready(_) => summary::LINE_COUNT,
        _ => 1,
    }
}

/// What a search runs against: everything the row shows.
fn row_text(row: &gh::PullRequestRow<'_>) -> String {
    let repository = row.repository.map(gh::Repo::slug).unwrap_or_default();

    format!("{repository} #{} {}", row.number, row.title)
}

/// Paints the selector immediately and fills it in when the listing lands, so
/// the wait happens inside the alternate screen rather than in front of it.
pub async fn select(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    repo: Option<gh::Repo>,
    theme: &mut Theme,
    follow_terminal: bool,
) -> Result<Option<PullRequestTarget>> {
    let (tx, mut rx) = mpsc::unbounded_channel();
    spawn_listing(repo, tx.clone());

    let mut selector = Selector::new();
    let mut animation = tokio::time::interval(Duration::from_millis(90));
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !selector.is_done {
        terminal::render(terminal, |frame| {
            draw(frame, &selector, *theme);
        })
        .context("drawing pull request selector")?;

        let viewport = viewport(terminal.get_frame().area(), &selector);

        let event = tokio::select! {
            _ = animation.tick(), if selector.is_waiting() => {
                selector.loading_frame =
                    selector.loading_frame.wrapping_add(1);
                continue;
            }
            message = rx.recv() => {
                let Some(message) = message else {
                    anyhow::bail!("selector message channel closed");
                };
                selector.receive(message, viewport);
                continue;
            }
            event = crate::next_event(events) => {
                event
                    .context("terminal event stream closed")?
                    .context("reading terminal event")?
            }
        };

        match event {
            Event::Key(key)
                if matches!(
                    key.kind,
                    KeyEventKind::Press | KeyEventKind::Repeat
                ) =>
            {
                if let Some(target) = selector.press(key, viewport) {
                    spawn_summary(target, tx.clone());
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

    Ok(selector.chosen)
}

/// Reads the listing behind the selector so the wait is spent in the alternate
/// screen rather than in front of it.
fn spawn_listing(repo: Option<gh::Repo>, tx: mpsc::UnboundedSender<Message>) {
    tokio::spawn(async move {
        let listed = match repo {
            Some(repo) => gh::repository_pull_requests(repo).await,
            None => gh::user_pull_requests().await,
        };

        let _ = tx.send(Message::Listed(listed));
    });
}

fn spawn_summary(
    target: PullRequestTarget,
    tx: mpsc::UnboundedSender<Message>,
) {
    tokio::spawn(async move {
        let summarized = gh::fetch_summary(&target.repo, target.number).await;
        let _ = tx.send(Message::Summarized(target, summarized));
    });
}

/// The rows a motion counts: the table's, or the panel's while one is open.
fn viewport(area: Rect, selector: &Selector) -> usize {
    let (body, _) = split(area);

    match selector.panel {
        Some(_) => panel_area(body).height.saturating_sub(4) as usize,
        None => body
            .height
            .saturating_sub(2)
            .saturating_sub(HEADER_ROWS)
            .into(),
    }
}

/// The list, and the status bar under it.
fn split(area: Rect) -> (Rect, Rect) {
    let panes = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(1), Constraint::Length(1)])
        .split(area);

    (panes[0], panes[1])
}

fn draw(frame: &mut Frame, selector: &Selector, theme: Theme) {
    let (area, status) = split(frame.area());
    draw_status(frame, selector, status, theme);

    let title = match selector.listing.rows() {
        Some(pull_requests) => {
            format!(" Open pull requests · {} ", pull_requests.len())
        }
        None => " Open pull requests ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::styled(
            title,
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);

    let pull_requests = match &selector.listing {
        Listing::Ready(pull_requests) => pull_requests,
        Listing::Loading => {
            frame.render_widget(block, area);
            draw_centered(
                frame,
                inner,
                spinner_line(
                    selector.loading_frame,
                    "loading pull requests",
                    theme,
                ),
            );
            return;
        }
        Listing::Failed(failure) => {
            frame.render_widget(block, area);
            draw_centered(
                frame,
                inner,
                Line::styled(
                    failure.clone(),
                    Style::default().fg(theme.danger),
                ),
            );
            return;
        }
    };

    draw_table(frame, selector, pull_requests, block, area, theme);

    if selector.visible.is_empty() {
        let empty = if selector.filter.is_empty() {
            "no open pull requests"
        } else {
            "nothing matches"
        };

        draw_centered(
            frame,
            inner,
            Line::styled(empty, Style::default().fg(theme.dim)),
        );
    }

    draw_panel(frame, selector, area, theme);
}

fn draw_table(
    frame: &mut Frame,
    selector: &Selector,
    pull_requests: &PullRequestList,
    block: Block<'static>,
    area: Rect,
    theme: Theme,
) {
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
    let rows = selector.visible.iter().filter_map(|index| {
        let choice = pull_requests.row(*index)?;
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

    let mut state = TableState::default()
        .with_selected(selector.selected())
        .with_offset(selector.cursor.scroll);
    frame.render_stateful_widget(table, area, &mut state);
}

/// The panel floats over the list rather than docking beside it: it is read,
/// and the row it belongs to is the one the cursor is already parked on.
fn panel_area(area: Rect) -> Rect {
    let width = PANEL_WIDTH.min(area.width);
    let height = (summary::LINE_COUNT as u16 + 4).min(area.height);

    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn draw_panel(
    frame: &mut Frame,
    selector: &Selector,
    area: Rect,
    theme: Theme,
) {
    let Some(panel) = selector.panel.as_ref() else {
        return;
    };

    let outer = panel_area(area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::styled(
            format!(" {} #{} ", panel.target.repo.slug(), panel.target.number),
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ));
    // A row of padding inside each border, so the panel reads as a panel
    // rather than as a second pane.
    let inner = Rect {
        x: outer.x + 2,
        y: outer.y + 2,
        width: outer.width.saturating_sub(4),
        height: outer.height.saturating_sub(4),
    };

    frame.render_widget(Clear, outer);
    frame.render_widget(block, outer);

    let lines = match &panel.state {
        PanelState::Ready(summary) => summary::build(summary, theme),
        PanelState::Loading => vec![spinner_line(
            selector.loading_frame,
            "loading the summary",
            theme,
        )],
        PanelState::Failed(failure) => vec![Line::styled(
            failure.clone(),
            Style::default().fg(theme.danger),
        )],
    };
    let visible: Vec<Line> = lines.into_iter().skip(panel.scroll).collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

/// The same bar the review surface wears, so the mode, the `/` line and the
/// keys sit where the reader already looks for them.
fn draw_status(
    frame: &mut Frame,
    selector: &Selector,
    area: Rect,
    theme: Theme,
) {
    let bar = ui::bar_style(theme);
    let mut spans = vec![ui::mode_chip(selector.mode, theme)];

    if selector.mode == Mode::Filter {
        spans.push(Span::styled("  /", bar.fg(theme.purple)));
        spans
            .push(Span::styled(selector.filter.clone(), bar.fg(theme.heading)));
    }

    spans.push(Span::styled(position(selector), bar.fg(theme.muted)));

    let pending = selector.keymap.pending_hint();
    if !pending.is_empty() {
        spans.push(Span::styled(
            format!("   {pending}"),
            bar.fg(theme.accent).add_modifier(Modifier::BOLD),
        ));
    }

    if selector.is_waiting() {
        spans.push(Span::styled(
            format!("   {}", SPINNER[selector.loading_frame % SPINNER.len()]),
            bar.fg(theme.accent),
        ));
    }

    if !selector.status.is_empty() {
        spans.push(Span::styled(
            format!("   {}", selector.status),
            bar.fg(theme.dim),
        ));
    }

    let left_width = Line::from(spans.clone()).width();
    ui::draw_status_bar(frame, area, spans, key_hints(selector), &[], theme);

    // The `/` line is typed on the bar, so the terminal cursor belongs there.
    if selector.mode == Mode::Filter {
        let column = left_width;
        if column < area.width as usize {
            frame.set_cursor_position((area.x + column as u16, area.y));
        }
    }
}

/// Where the cursor is in the list, counted against what the filter left.
fn position(selector: &Selector) -> String {
    if selector.visible.is_empty() {
        return String::new();
    }

    let shown = selector.visible.len();
    let where_in = format!("  {}/{shown}", selector.cursor.index + 1);

    if selector.filter.is_empty() {
        return where_in;
    }

    format!("{where_in} of {}", selector.listing.len())
}

fn key_hints(selector: &Selector) -> &'static [(&'static str, &'static str)] {
    if selector.mode == Mode::Filter {
        return &[("↑↓", "select"), ("↵", "apply"), ("esc", "cancel")];
    }

    if selector.panel.is_some() {
        return &[("j/k", "scroll"), ("↵", "open"), ("esc", "close")];
    }

    if !selector.filter.is_empty() {
        return &[
            ("j/k", "move"),
            ("K", "summary"),
            ("↵", "open"),
            ("esc", "clear"),
        ];
    }

    &[
        ("j/k", "move"),
        ("K", "summary"),
        ("↵", "open"),
        ("/", "filter"),
        ("q", "quit"),
    ]
}

fn spinner_line(
    loading_frame: usize,
    label: &'static str,
    theme: Theme,
) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            SPINNER[loading_frame % SPINNER.len()],
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!("  {label}"), Style::default().fg(theme.dim)),
    ])
}

fn draw_centered(frame: &mut Frame, area: Rect, line: Line<'static>) {
    if area.is_empty() {
        return;
    }

    frame.render_widget(
        Paragraph::new(line).alignment(Alignment::Center),
        Rect {
            y: area.y + area.height.saturating_sub(1) / 2,
            height: 1,
            ..area
        },
    );
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

    fn ready(pull_requests: PullRequestList) -> Selector {
        let mut selector = Selector::new();
        selector.receive(Message::Listed(Ok(pull_requests)), 10);

        selector
    }

    fn many() -> PullRequestList {
        PullRequestList::User {
            pulls: (1..=40)
                .map(|number| {
                    located(
                        number,
                        &format!("Change {number}"),
                        ReviewStatus::Approved,
                    )
                })
                .collect(),
        }
    }

    fn press(selector: &mut Selector, chord: &str) {
        for character in chord.chars() {
            press_key(selector, KeyCode::Char(character), Modifiers::NONE);
        }
    }

    fn press_key(selector: &mut Selector, code: KeyCode, modifiers: Modifiers) {
        selector.press(KeyEvent::new(code, modifiers), 10);
    }

    fn render(pull_requests: PullRequestList) -> String {
        render_selector(&ready(pull_requests))
    }

    fn render_selector(selector: &Selector) -> String {
        let mut terminal = Terminal::new(TestBackend::new(110, 20)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, selector, Theme::dark());
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
        let rendered = render(pull_requests);

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
        let rendered = render(pull_requests);

        assert!(!rendered.contains("REPOSITORY"));
        assert!(rendered.contains("Local change"));
    }

    #[test]
    fn the_dashboard_spins_until_the_listing_lands() {
        let rendered = render_selector(&Selector::new());

        assert!(rendered.contains("Open pull requests"));
        assert!(!rendered.contains("Open pull requests ·"));
        assert!(rendered.contains("loading pull requests"));
        assert!(rendered.contains(SPINNER[0]));
    }

    #[test]
    fn a_failed_listing_stays_on_screen() {
        let mut selector = Selector::new();
        selector.receive(
            Message::Listed(Err(anyhow::anyhow!("gh pr list failed"))),
            10,
        );

        assert!(
            render_selector(&selector).contains("error: gh pr list failed")
        );
    }

    #[test]
    fn an_empty_dashboard_cannot_select_a_pull_request() {
        let pull_requests = PullRequestList::Repository {
            repo: Repo::parse("owner/repo").unwrap(),
            pulls: Vec::new(),
        };
        assert!(pull_requests.select(0).is_none());
    }

    /// The whole point of the shared keymap: a count means here what it means
    /// in the diff.
    #[test]
    fn a_count_jumps_to_the_row_it_names() {
        let mut selector = ready(many());

        press(&mut selector, "12G");
        assert_eq!(selector.cursor.index, 11);

        press(&mut selector, "gg");
        assert_eq!(selector.cursor.index, 0);

        press(&mut selector, "3j");
        assert_eq!(selector.cursor.index, 3);

        press(&mut selector, "G");
        assert_eq!(selector.cursor.index, 39);
    }

    #[test]
    fn a_half_page_scroll_moves_by_the_viewport() {
        let mut selector = ready(many());

        press_key(&mut selector, KeyCode::Char('d'), Modifiers::CONTROL);
        assert_eq!(selector.cursor.index, 5);

        press_key(&mut selector, KeyCode::Char('u'), Modifiers::CONTROL);
        assert_eq!(selector.cursor.index, 0);
    }

    #[test]
    /// The point of a filter: the list narrows on the keystroke, not on the
    /// `<CR>` that ends the line.
    fn the_list_narrows_as_the_filter_is_typed() {
        let mut selector = ready(many());

        press(&mut selector, "/");
        assert_eq!(selector.visible.len(), 40);

        press(&mut selector, "Change 3");
        assert_eq!(selector.visible.len(), 11);
        assert_eq!(selector.target().map(|target| target.number), Some(3));

        press_key(&mut selector, KeyCode::Backspace, Modifiers::NONE);
        assert_eq!(selector.visible.len(), 40);
    }

    #[test]
    fn accepting_the_filter_keeps_the_narrowed_list() {
        let mut selector = ready(many());

        press(&mut selector, "/");
        press(&mut selector, "Change 30");
        press_key(&mut selector, KeyCode::Enter, Modifiers::NONE);

        assert_eq!(selector.mode, Mode::Normal);
        assert_eq!(selector.visible.len(), 1);
        assert_eq!(selector.target().map(|target| target.number), Some(30));
    }

    /// Cancelling puts back both the list and the row it was opened on.
    #[test]
    fn cancelling_the_filter_restores_the_row_it_opened_on() {
        let mut selector = ready(many());

        press(&mut selector, "7G");
        press(&mut selector, "/");
        press(&mut selector, "Change 12");
        press_key(&mut selector, KeyCode::Escape, Modifiers::NONE);

        assert_eq!(selector.mode, Mode::Normal);
        assert_eq!(selector.visible.len(), 40);
        assert_eq!(selector.target().map(|target| target.number), Some(7));
    }

    /// Motions count the rows on screen, not the ones the filter hid.
    #[test]
    fn motions_walk_the_narrowed_list() {
        let mut selector = ready(many());

        press(&mut selector, "/");
        press(&mut selector, "Change 1");
        press_key(&mut selector, KeyCode::Enter, Modifiers::NONE);
        press(&mut selector, "G");

        assert_eq!(selector.target().map(|target| target.number), Some(19));
    }

    #[test]
    fn a_filter_that_matches_nothing_says_so() {
        let mut selector = ready(many());

        press(&mut selector, "/");
        press(&mut selector, "nothing here");

        assert!(selector.visible.is_empty());
        assert!(selector.target().is_none());
        assert!(render_selector(&selector).contains("nothing matches"));
    }

    #[test]
    fn the_bar_carries_the_mode_the_position_and_the_keys() {
        let mut selector = ready(many());
        press(&mut selector, "3G");
        let rendered = render_selector(&selector);

        assert!(rendered.contains("NORMAL"));
        assert!(rendered.contains("3/40"));
        assert!(rendered.contains("K summary"));

        press(&mut selector, "/");
        press(&mut selector, "Change 2");
        let filtering = render_selector(&selector);

        assert!(filtering.contains("FILTER"));
        assert!(filtering.contains("/Change 2"));
        assert!(filtering.contains("1/11 of 40"));
        assert!(filtering.contains("esc cancel"));
    }

    /// `K` is the panel's key both ways, the way `o` is on the review surface.
    #[test]
    fn the_summary_panel_opens_on_the_row_under_the_cursor() {
        let mut selector = ready(many());

        press(&mut selector, "5G");
        let asked = selector.apply(&Action::OpenOverview, 10);

        assert_eq!(asked.map(|target| target.number), Some(5));
        assert_eq!(selector.mode, Mode::Overview);
        assert!(render_selector(&selector).contains("loading the summary"));

        press(&mut selector, "K");
        assert!(selector.panel.is_none());
        assert_eq!(selector.mode, Mode::Normal);
    }

    #[test]
    fn the_panel_shows_the_summary_once_it_lands() {
        let mut selector = ready(many());
        let target = selector.apply(&Action::OpenOverview, 10).unwrap();
        selector.receive(
            Message::Summarized(
                target,
                Ok(Summary {
                    author: "tale".into(),
                    base_ref: "main".into(),
                    head_ref: "rows".into(),
                    additions: 120,
                    deletions: 34,
                    changed_files: 7,
                    updated_on: "2026-08-20".into(),
                    comments: 3,
                    checks: gh::Checks {
                        passed: 12,
                        failed: 1,
                        running: 0,
                        skipped: 0,
                    },
                    reviews: gh::Reviews {
                        approved: 1,
                        changes_requested: 0,
                        commented: 0,
                        requested: 2,
                    },
                    threads: gh::Threads {
                        unresolved: 4,
                        total: 11,
                        is_truncated: false,
                    },
                }),
            ),
            10,
        );
        let rendered = render_selector(&selector);

        assert!(rendered.contains("owner/repo #1"));
        assert!(rendered.contains("12 passed"));
        assert!(rendered.contains("4 unresolved of 11"));
    }

    /// The cursor cannot wander off the pull request the panel is pinned to,
    /// so motions scroll the panel while it is open.
    #[test]
    fn motions_scroll_the_panel_rather_than_the_list() {
        let mut selector = ready(many());
        selector.apply(&Action::OpenOverview, 10);
        let row = selector.cursor.index;

        press(&mut selector, "j");

        assert_eq!(selector.cursor.index, row);
    }

    #[test]
    fn enter_opens_the_pull_request_under_the_cursor() {
        let mut selector = ready(many());

        press(&mut selector, "7G");
        press_key(&mut selector, KeyCode::Enter, Modifiers::NONE);

        assert!(selector.is_done);
        assert_eq!(selector.chosen.map(|target| target.number), Some(7));
    }

    #[test]
    fn escape_clears_the_filter_before_it_quits() {
        let mut selector = ready(many());

        press(&mut selector, "/");
        press(&mut selector, "Change 3");
        press_key(&mut selector, KeyCode::Enter, Modifiers::NONE);

        press_key(&mut selector, KeyCode::Escape, Modifiers::NONE);
        assert!(!selector.is_done);
        assert!(selector.filter.is_empty());
        assert_eq!(selector.visible.len(), 40);

        press_key(&mut selector, KeyCode::Escape, Modifiers::NONE);
        assert!(selector.is_done);
    }
}
