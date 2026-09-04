//! The landing page: every open pull request, and a summary of the one under
//! the cursor.
//!
//! Keys reach it through the same keymap the review surface uses, so a count, a
//! half-page scroll and a search mean here what they mean there.

use crate::app::action::Action;
use crate::app::keymap::{Keymap, Resolution};
use crate::app::mode::Mode;
use crate::app::search::Query;
use crate::layout::{panel_area, panel_inner};
use crate::overview::{self, FoldState};
use crate::renderer::Theme;
use crate::ui::{self, SPINNER};
use crate::vim::Cursor;
#[cfg(test)]
use prtui_core::Repo;
use prtui_core::{
    PullRequestList, PullRequestListItem, PullRequestListScope,
    PullRequestOverview, PullRequestTarget, ReviewStatus,
};
use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, HighlightSpacing, Paragraph, Row,
    Table, TableState,
};
use std::sync::Arc;
use termina::event::{KeyCode, KeyEvent, Modifiers};

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
    ("no", "gx", "open"),
    ("no", "<CR>", "activate"),
    ("n", "q", "quit"),
    ("n", "<Esc>", "escape"),
    ("o", "za", "expand-all"),
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

/// Result of work the selector asked the runtime to execute.
pub enum Message {
    /// A completed pull request listing.
    Listed(Result<PullRequestList, String>),
    /// A completed overview for the named pull request.
    Overview(Arc<PullRequestTarget>, Result<PullRequestOverview, String>),
    /// An external action that failed.
    Failed(String),
}

/// Work the selector delegates to the runtime.
pub enum Effect {
    /// Fetch an overview for a pull request.
    FetchOverview(Arc<PullRequestTarget>),
    /// Open a pull request in the browser.
    Open(PullRequestTarget),
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

/// The overview panel, pinned to the pull request it was opened on the way an
/// editor's hover is pinned to the symbol under the cursor.
struct Panel {
    target: Arc<PullRequestTarget>,
    /// The table is covered while the panel is open, so keep the identity the
    /// reader selected in the panel itself.
    title: String,
    state: PanelState,
    /// The line the panel is reading, which is what the motions move and the
    /// frame scrolls to follow.
    cursor: Cursor,
    /// A busy repository reports dozens of checks, so they open on the tally
    /// and the list is asked for with `za`.
    folds: FoldState,
}

enum PanelState {
    Loading,
    Ready(Box<PullRequestOverview>),
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

impl Default for Selector {
    fn default() -> Self {
        Self::new()
    }
}

impl Selector {
    /// Creates a selector waiting for its initial listing.
    pub fn new() -> Self {
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

    /// Whether a loading animation should advance.
    pub const fn is_waiting(&self) -> bool {
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

        Some(self.listing.rows()?.get(row)?.target.clone())
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
                    pull_requests.get(*index).is_some_and(|item| {
                        query.is_match(&row_text(item, pull_requests.scope))
                    })
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

    /// Applies one runtime result.
    pub fn receive(&mut self, message: Message, metrics: Metrics) {
        let viewport = metrics.viewport;
        match message {
            Message::Listed(Ok(pull_requests)) => {
                self.listing = Listing::Ready(pull_requests);
                self.sync_visible(viewport);
            }
            Message::Listed(Err(err)) => {
                self.listing = Listing::Failed(format!("error: {err}"));
            }
            Message::Overview(target, overview) => {
                self.set_overview(&target, overview);
            }
            Message::Failed(error) => self.status = format!("error: {error}"),
        }
    }

    /// A summary that arrives after the panel moved on belongs to nothing on
    /// screen, so it is dropped rather than painted over the panel that
    /// replaced it.
    fn set_overview(
        &mut self,
        target: &PullRequestTarget,
        overview: Result<PullRequestOverview, String>,
    ) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };

        if panel.target.number != target.number
            || panel.target.repo.slug() != target.repo.slug()
        {
            return;
        }

        panel.state = match overview {
            Ok(overview) => PanelState::Ready(Box::new(overview)),
            Err(err) => PanelState::Failed(format!("error: {err}")),
        };
    }

    /// One keystroke, resolved against the keymap and applied. Answers with the
    /// pull request to summarize, which the caller fetches.
    /// Applies one key and returns any work it requests.
    pub fn press(&mut self, key: KeyEvent, metrics: Metrics) -> Option<Effect> {
        match self.keymap.resolve(self.mode, key) {
            Resolution::Action(action) => self.apply(&action, metrics),
            Resolution::Pending => None,
            Resolution::Unbound => {
                self.type_key(key, metrics.viewport);
                None
            }
        }
    }

    fn apply(&mut self, action: &Action, metrics: Metrics) -> Option<Effect> {
        let viewport = metrics.viewport;
        match action {
            Action::Move(motion) => {
                match self.panel.as_mut() {
                    Some(panel) => {
                        let len =
                            panel_len(panel, metrics.width, metrics.theme);
                        panel.cursor.apply(*motion, len, viewport);
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
            // Inside the panel `<CR>` opens what the cursor is on, which is
            // the fold on its own row and the pull request everywhere else.
            Action::Activate
                if self.is_on_fold(metrics.width, metrics.theme) =>
            {
                self.toggle_fold(metrics);
            }
            Action::Activate => {
                self.chosen = self.target();
                self.is_done = self.chosen.is_some();
            }
            Action::Quit => self.is_done = true,
            Action::Escape => {
                if self.filter.is_empty() {
                    self.status = "press q to quit".into();
                    return None;
                }

                self.filter.clear();
                self.sync_visible(viewport);
            }
            Action::OpenOverview => {
                return self.open_panel().map(Effect::FetchOverview);
            }
            Action::OpenInBrowser => {
                let target = self.target()?;
                return Some(Effect::Open(target));
            }
            Action::CloseOverlay => self.close_panel(),
            Action::Expand(_) => self.toggle_fold(metrics),
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

    fn open_panel(&mut self) -> Option<Arc<PullRequestTarget>> {
        let row = *self.visible.get(self.cursor.index)?;
        let pull_requests = self.listing.rows()?;
        let item = pull_requests.get(row)?;
        let title = item.title.clone();
        let target = Arc::new(item.target.clone());

        self.panel = Some(Panel {
            target: Arc::clone(&target),
            title,
            state: PanelState::Loading,
            cursor: Cursor::default(),
            folds: FoldState::default(),
        });
        self.set_mode(Mode::Overview);

        Some(target)
    }

    /// Whether the panel's cursor is parked on the checks fold.
    fn is_on_fold(&self, width: usize, theme: Theme) -> bool {
        let Some(panel) = self.panel.as_ref() else {
            return false;
        };
        let PanelState::Ready(_) = &panel.state else {
            return false;
        };

        panel_rows(panel, width, theme, self.loading_frame)
            .fold_at(panel.cursor.index)
            .is_some()
    }

    fn toggle_fold(&mut self, metrics: Metrics) {
        let Some(panel) = self.panel.as_mut() else {
            return;
        };
        let rows =
            panel_rows(panel, metrics.width, metrics.theme, self.loading_frame);
        let Some(fold) = rows.fold_at(panel.cursor.index).cloned() else {
            return;
        };

        panel.folds.toggle(&fold);

        let len = panel_len(panel, metrics.width, metrics.theme);
        panel.cursor.jump(panel.cursor.index, len, metrics.viewport);
    }

    fn close_panel(&mut self) {
        self.panel = None;
        self.set_mode(Mode::Normal);
    }

    /// A review returns to the list exactly where it left it. Only transient
    /// panel state is discarded; the listing, filter and cursor remain useful.
    /// Restores the list after returning from a review.
    pub fn resume(&mut self) {
        self.is_done = false;
        self.chosen = None;
        self.panel = None;
        self.status.clear();
        self.set_mode(Mode::Normal);
    }

    /// Advances the loading animation by one frame.
    pub const fn advance_loading(&mut self) {
        self.loading_frame = self.loading_frame.wrapping_add(1);
    }

    /// Whether selection has finished for this visit.
    pub const fn is_done(&self) -> bool {
        self.is_done
    }

    /// Takes the pull request chosen by the reader.
    pub const fn take_chosen(&mut self) -> Option<PullRequestTarget> {
        self.chosen.take()
    }
}

fn panel_len(panel: &Panel, width: usize, theme: Theme) -> usize {
    panel_rows(panel, width, theme, 0).len()
}

fn panel_rows(
    panel: &Panel,
    width: usize,
    theme: Theme,
    loading_frame: usize,
) -> overview::Rows {
    match &panel.state {
        PanelState::Ready(value) => overview::build(
            &value.summary,
            &value.body,
            &value.discussion,
            &panel.folds,
            width,
            theme,
        ),
        PanelState::Loading => overview::Rows {
            lines: vec![spinner_line(
                loading_frame,
                "loading the overview",
                theme,
            )],
            folds: vec![None],
        },
        PanelState::Failed(failure) => overview::Rows {
            lines: vec![Line::styled(
                failure.clone(),
                Style::default().fg(theme.danger),
            )],
            folds: vec![None],
        },
    }
}

/// What a search runs against: everything the row shows.
fn row_text(item: &PullRequestListItem, scope: PullRequestListScope) -> String {
    let repository = if scope == PullRequestListScope::User {
        item.target.repo.slug()
    } else {
        String::new()
    };

    format!(
        "{repository} #{} {} @{}",
        item.target.number, item.title, item.author,
    )
}

/// Frame dimensions needed to route selector input.
#[derive(Clone, Copy)]
pub struct Metrics {
    viewport: usize,
    width: usize,
    theme: Theme,
}

/// Measures the active selector surface.
pub fn metrics(area: Rect, selector: &Selector, theme: Theme) -> Metrics {
    let (body, _) = split(area);

    let (viewport, width) = match selector.panel {
        Some(_) => {
            let inner = panel_inner(panel_area(body));
            (inner.height as usize, inner.width as usize)
        }
        None => (
            body.height
                .saturating_sub(2)
                .saturating_sub(HEADER_ROWS)
                .into(),
            0,
        ),
    };

    Metrics {
        viewport,
        width,
        theme,
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

/// Draws the selector without mutating its state.
pub fn draw(frame: &mut Frame, selector: &Selector, theme: Theme) {
    let (area, status) = split(frame.area());
    let metrics = metrics(frame.area(), selector, theme);
    draw_status(frame, selector, status, metrics, theme);

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

    draw_panel(frame, selector, area, metrics, theme);
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
    let has_repository_column =
        pull_requests.scope == PullRequestListScope::User;
    if has_repository_column {
        headers.push(Cell::from("REPOSITORY"));
        widths.push(Constraint::Length(28));
    }
    headers.extend([
        Cell::from("PR"),
        Cell::from("AUTHOR"),
        Cell::from("TITLE"),
    ]);
    widths.extend([
        Constraint::Length(8),
        Constraint::Length(16),
        Constraint::Fill(1),
    ]);

    let header = Row::new(headers)
        .style(
            Style::default()
                .fg(theme.muted)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);
    let rows = selector.visible.iter().filter_map(|index| {
        let item = pull_requests.get(*index)?;
        let mut cells = vec![review_cell(&item.review_status, theme)];
        if has_repository_column {
            cells.push(
                Cell::from(item.target.repo.slug())
                    .style(Style::default().fg(theme.muted)),
            );
        }
        cells.extend([
            Cell::from(format!("#{}", item.target.number))
                .style(Style::default().fg(theme.warning)),
            Cell::from(item.author.as_str())
                .style(Style::default().fg(theme.accent)),
            Cell::from(item.title.as_str())
                .style(Style::default().fg(theme.code)),
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

fn draw_panel(
    frame: &mut Frame,
    selector: &Selector,
    area: Rect,
    metrics: Metrics,
    theme: Theme,
) {
    let Some(panel) = selector.panel.as_ref() else {
        return;
    };

    let outer = panel_area(area);
    let rows = panel_rows(panel, metrics.width, theme, selector.loading_frame);
    let actions = if rows.fold_at(panel.cursor.index).is_some() {
        " ↵/za toggle · gx browser · esc close "
    } else {
        " ↵ review · gx browser · esc close "
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(theme.accent))
        .title(Line::styled(
            format!(
                " {} #{} · {} ",
                panel.target.repo.slug(),
                panel.target.number,
                panel.title
            ),
            Style::default()
                .fg(theme.heading)
                .add_modifier(Modifier::BOLD),
        ))
        .title_bottom(
            Line::styled(actions, Style::default().fg(theme.dim))
                .right_aligned(),
        );
    // A row of padding inside each border, so the panel reads as a panel
    // rather than as a second pane.
    let inner = panel_inner(outer);

    frame.render_widget(Clear, outer);
    frame.render_widget(block, outer);

    // The cursor is a painted row rather than a terminal cursor, the way the
    // tree and the diff carry theirs.
    let width = inner.width as usize;
    let visible: Vec<Line> = rows
        .lines
        .into_iter()
        .enumerate()
        .skip(panel.cursor.scroll)
        .take(inner.height as usize)
        .map(|(row, line)| {
            if row == panel.cursor.index {
                ui::cursor_line(line, width, theme)
            } else {
                line
            }
        })
        .collect();

    frame.render_widget(Paragraph::new(visible), inner);
}

/// The same bar the review surface wears, so the mode, the `/` line and the
/// keys sit where the reader already looks for them.
fn draw_status(
    frame: &mut Frame,
    selector: &Selector,
    area: Rect,
    metrics: Metrics,
    theme: Theme,
) {
    let bar = ui::bar_style(theme);
    let mut spans = vec![ui::mode_chip(selector.mode, theme)];

    if selector.mode == Mode::Filter {
        spans.push(Span::styled("  /", bar.fg(theme.purple)));
        spans
            .push(Span::styled(selector.filter.clone(), bar.fg(theme.heading)));
    }

    spans.push(Span::styled(
        position(selector, metrics),
        bar.fg(theme.muted),
    ));

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
    ui::draw_status_bar(
        frame,
        area,
        spans,
        key_hints(selector, metrics),
        &[],
        theme,
    );

    // The `/` line is typed on the bar, so the terminal cursor belongs there.
    if selector.mode == Mode::Filter {
        let column = left_width;
        if column < area.width as usize {
            frame.set_cursor_position((area.x + column as u16, area.y));
        }
    }
}

/// Where the cursor is: in the panel while one is open, otherwise in the list,
/// counted against what the filter left.
fn position(selector: &Selector, metrics: Metrics) -> String {
    if let Some(panel) = selector.panel.as_ref() {
        return format!(
            "  {}/{}",
            panel.cursor.index + 1,
            panel_len(panel, metrics.width, metrics.theme)
        );
    }

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

fn key_hints(
    selector: &Selector,
    metrics: Metrics,
) -> &'static [(&'static str, &'static str)] {
    if selector.mode == Mode::Filter {
        return &[("↑↓", "select"), ("↵", "apply"), ("esc", "cancel")];
    }

    if selector.panel.is_some() {
        if selector.is_on_fold(metrics.width, metrics.theme) {
            return &[
                ("j/k", "move"),
                ("↵/za", "toggle"),
                ("gx", "browser"),
                ("esc", "close"),
            ];
        }

        return &[
            ("j/k", "move"),
            ("↵", "review"),
            ("gx", "browser"),
            ("esc", "close"),
        ];
    }

    if !selector.filter.is_empty() {
        return &[
            ("j/k", "move"),
            ("K", "summary"),
            ("↵", "review"),
            ("gx", "browser"),
            ("esc", "clear"),
        ];
    }

    &[
        ("j/k", "move"),
        ("K", "summary"),
        ("↵", "review"),
        ("gx", "browser"),
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
    use prtui_core as model;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn pull(
        number: u32,
        title: &str,
        review_status: ReviewStatus,
    ) -> PullRequestListItem {
        PullRequestListItem {
            target: PullRequestTarget {
                repo: Arc::new(Repo {
                    host: None,
                    namespace: "owner".into(),
                    name: "repo".into(),
                }),
                number,
            },
            title: title.into(),
            author: "alice".into(),
            review_status,
        }
    }

    fn list(
        scope: PullRequestListScope,
        items: Vec<PullRequestListItem>,
    ) -> PullRequestList {
        PullRequestList { scope, items }
    }

    fn ready(pull_requests: PullRequestList) -> Selector {
        let mut selector = Selector::new();
        let metrics = frame_metrics(&selector);
        selector.receive(Message::Listed(Ok(pull_requests)), metrics);

        selector
    }

    fn many() -> PullRequestList {
        list(
            PullRequestListScope::User,
            (1..=40)
                .map(|number| {
                    pull(
                        number,
                        &format!("Change {number}"),
                        ReviewStatus::Approved,
                    )
                })
                .collect(),
        )
    }

    fn press(selector: &mut Selector, chord: &str) {
        for character in chord.chars() {
            press_key(selector, KeyCode::Char(character), Modifiers::NONE);
        }
    }

    fn press_key(selector: &mut Selector, code: KeyCode, modifiers: Modifiers) {
        selector.press(KeyEvent::new(code, modifiers), frame_metrics(selector));
    }

    fn render(pull_requests: PullRequestList) -> String {
        render_selector(&ready(pull_requests))
    }

    fn render_selector(selector: &Selector) -> String {
        let mut terminal =
            Terminal::new(TestBackend::new(FRAME.width, FRAME.height)).unwrap();
        terminal
            .draw(|frame| {
                draw(frame, selector, Theme::dark());
            })
            .unwrap();

        terminal.backend().to_string()
    }

    const FRAME: Rect = Rect {
        x: 0,
        y: 0,
        width: 110,
        height: 20,
    };

    fn frame_metrics(selector: &Selector) -> Metrics {
        metrics(FRAME, selector, Theme::dark())
    }

    #[test]
    fn dashboard_uses_the_full_frame_and_shows_every_review_status() {
        let pull_requests = list(
            PullRequestListScope::User,
            vec![
                pull(1, "Draft", ReviewStatus::Draft),
                pull(2, "Needs work", ReviewStatus::ChangesRequested),
                pull(3, "Needs review", ReviewStatus::ReviewRequired),
                pull(4, "Ready", ReviewStatus::Approved),
                pull(5, "Pending", ReviewStatus::NoDecision),
            ],
        );
        let rendered = render(pull_requests);

        assert!(rendered.contains("Open pull requests · 5"));
        assert!(rendered.contains("REVIEW"));
        assert!(rendered.contains("REPOSITORY"));
        assert!(rendered.contains("AUTHOR"));
        assert!(rendered.contains("alice"));
        assert!(rendered.contains("DRAFT"));
        assert!(rendered.contains("CHANGES REQUESTED"));
        assert!(rendered.contains("REVIEW REQUIRED"));
        assert!(rendered.contains("APPROVED"));
        assert!(rendered.contains("NO DECISION"));
        assert!(rendered.contains("owner/repo"));
    }

    #[test]
    fn repository_dashboard_omits_the_redundant_repository_column() {
        let pull_requests = list(
            PullRequestListScope::Repository,
            vec![pull(42, "Local change", ReviewStatus::Approved)],
        );
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
        let metrics = frame_metrics(&selector);
        selector
            .receive(Message::Listed(Err("gh pr list failed".into())), metrics);

        assert!(
            render_selector(&selector).contains("error: gh pr list failed")
        );
    }

    #[test]
    fn an_empty_dashboard_cannot_select_a_pull_request() {
        let pull_requests = list(PullRequestListScope::Repository, Vec::new());
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
        let half = frame_metrics(&selector).viewport / 2;

        press_key(&mut selector, KeyCode::Char('d'), Modifiers::CONTROL);
        assert_eq!(selector.cursor.index, half);

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
        let Some(Effect::FetchOverview(asked)) =
            selector.apply(&Action::OpenOverview, frame_metrics(&selector))
        else {
            panic!("K did not request a summary");
        };

        assert_eq!(asked.number, 5);
        assert_eq!(selector.mode, Mode::Overview);
        assert!(render_selector(&selector).contains("loading the overview"));

        press(&mut selector, "K");
        assert!(selector.panel.is_none());
        assert_eq!(selector.mode, Mode::Normal);
    }

    fn summary() -> model::Summary {
        model::Summary {
            author: "tale".into(),
            base_ref: "main".into(),
            head_ref: "rows".into(),
            additions: 120,
            deletions: 34,
            changed_files: 7,
            updated_on: "2026-08-20".into(),
            comments: 3,
            checks: vec![
                model::Check {
                    name: "clippy".into(),
                    state: model::CheckState::Failed,
                },
                model::Check {
                    name: "build".into(),
                    state: model::CheckState::Passed,
                },
            ],
            reviewers: vec![
                model::Reviewer {
                    name: "bob".into(),
                    is_team: false,
                    verdict: model::Verdict::ChangesRequested,
                },
                model::Reviewer {
                    name: "owner/backend".into(),
                    is_team: true,
                    verdict: model::Verdict::Waiting,
                },
            ],
            threads: model::Threads {
                unresolved: 4,
                total: 11,
                is_truncated: false,
            },
        }
    }

    fn overview_ready(selector: &mut Selector) {
        let Some(Effect::FetchOverview(target)) =
            selector.apply(&Action::OpenOverview, frame_metrics(selector))
        else {
            panic!("K did not request a summary");
        };
        let overview = PullRequestOverview {
            summary: summary(),
            body: "Why this change exists.".into(),
            discussion: vec![model::Comment {
                id: "IC_1".into(),
                reply_target: None,
                author: "alice".into(),
                body: "ship it".into(),
                created_at: "2026-09-03T10:00:00Z".into(),
                is_pending: false,
            }],
        };
        selector.receive(
            Message::Overview(target, Ok(overview)),
            frame_metrics(selector),
        );
    }

    #[test]
    fn the_panel_names_the_reviewers_and_folds_the_checks() {
        let mut selector = ready(many());
        overview_ready(&mut selector);
        let rendered = render_selector(&selector);

        assert!(rendered.contains("owner/repo #1"));
        assert!(rendered.contains("Change 1"));
        assert!(rendered.contains("↵ review"));
        assert!(rendered.contains("esc close"));
        assert!(rendered.contains("@bob"));
        assert!(rendered.contains("@owner/backend (team)"));
        assert!(rendered.contains("1 failed · 1 passed"));
        assert!(!rendered.contains("clippy"));

        press(&mut selector, "G");
        let rendered = render_selector(&selector);
        assert!(rendered.contains("@alice · 2026-09-03"));
        assert!(!rendered.contains("ship it"));
    }

    #[test]
    fn enter_opens_the_comment_under_the_cursor() {
        let mut selector = ready(many());
        overview_ready(&mut selector);

        press(&mut selector, "G");
        press_key(&mut selector, KeyCode::Enter, Modifiers::NONE);

        assert!(render_selector(&selector).contains("ship it"));
        assert!(!selector.is_done);
    }

    /// The fold is what the cursor is on, so `<CR>` opens it there and the
    /// hints say so.
    #[test]
    fn the_checks_open_from_the_row_the_cursor_is_on() {
        let mut selector = ready(many());
        overview_ready(&mut selector);

        press(&mut selector, "7G");
        let metrics = frame_metrics(&selector);
        assert!(selector.is_on_fold(metrics.width, metrics.theme));

        press_key(&mut selector, KeyCode::Enter, Modifiers::NONE);
        assert!(!selector.is_done);

        let rendered = render_selector(&selector);

        assert!(rendered.contains("clippy"));
        assert!(rendered.contains("build"));
        assert!(rendered.contains("↵/za toggle"));
    }

    /// `za` toggles the fold under the cursor.
    #[test]
    fn za_folds_the_checks_under_the_cursor() {
        let mut selector = ready(many());
        overview_ready(&mut selector);

        press(&mut selector, "7G");
        press(&mut selector, "za");
        assert!(render_selector(&selector).contains("clippy"));

        press(&mut selector, "za");
        assert!(!render_selector(&selector).contains("clippy"));
    }

    /// The panel's cursor is a row of its own, which the frame follows.
    #[test]
    fn the_panel_carries_a_cursor_rather_than_a_scroll() {
        let mut selector = ready(many());
        overview_ready(&mut selector);

        press(&mut selector, "3j");
        let panel = selector.panel.as_ref().unwrap();

        assert_eq!(panel.cursor.index, 3);
        assert_eq!(panel.cursor.scroll, 0);
    }

    /// The cursor cannot wander off the pull request the panel is pinned to,
    /// so motions scroll the panel while it is open.
    #[test]
    fn motions_scroll_the_panel_rather_than_the_list() {
        let mut selector = ready(many());
        selector.apply(&Action::OpenOverview, frame_metrics(&selector));
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
    fn gx_opens_the_selected_pull_request_in_the_browser() {
        let mut selector = ready(many());

        press_key(&mut selector, KeyCode::Char('g'), Modifiers::NONE);
        let effect = selector.press(
            KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE),
            frame_metrics(&selector),
        );

        let Some(Effect::Open(target)) = effect else {
            panic!("gx did not produce a browser action");
        };
        assert_eq!(target.repo.slug(), "owner/repo");
        assert_eq!(target.number, 1);
    }

    #[test]
    fn returning_from_a_review_keeps_the_selected_row() {
        let mut selector = ready(many());

        press(&mut selector, "7G");
        press_key(&mut selector, KeyCode::Enter, Modifiers::NONE);
        selector.resume();

        assert!(!selector.is_done);
        assert_eq!(selector.target().map(|target| target.number), Some(7));
    }

    #[test]
    fn escape_clears_the_filter_but_only_q_quits() {
        let mut selector = ready(many());

        press(&mut selector, "/");
        press(&mut selector, "Change 3");
        press_key(&mut selector, KeyCode::Enter, Modifiers::NONE);

        press_key(&mut selector, KeyCode::Escape, Modifiers::NONE);
        assert!(!selector.is_done);
        assert!(selector.filter.is_empty());
        assert_eq!(selector.visible.len(), 40);

        press_key(&mut selector, KeyCode::Escape, Modifiers::NONE);
        assert!(!selector.is_done);
        assert_eq!(selector.status, "press q to quit");

        press(&mut selector, "q");
        assert!(selector.is_done);
    }
}
