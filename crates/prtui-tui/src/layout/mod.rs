pub mod rows;
pub mod tree;

use crate::app::SummaryState;
use crate::app::View as AppView;
use crate::app::keymap::Reference;
use crate::app::mode::Mode;
use crate::app::search::Query;
use crate::expand::Gap;
use crate::overview;
use crate::ui::SPINNER;
use ratatui::layout::{Constraint, Direction, Rect};
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use rows::{Rows, View as RowView};
use tree::Tree;

/// Narrowest the diff is allowed to get before the tree stops taking room.
const MIN_DIFF_WIDTH: u16 = 20;

/// This represents the app's layout as a function of its state. It computes
/// details which are passed to the renderer, cleanly bridging the app state and
/// the UI without ugly concern mixing.
pub struct Layout {
    pub header: Rect,
    pub body: Rect,
    pub status: Rect,
    /// The tree's outer area, border included. Absent while the tree is hidden.
    pub files_pane: Option<Rect>,
    /// The tree's scrolling list, below the filter prompt when one is open.
    pub files_list: Option<Rect>,
    /// The single row the filter query is typed on, when one is open.
    pub files_prompt: Option<Rect>,
    /// The diff's outer area, including the rule its title sits on.
    pub diff_pane: Rect,
    pub diff: Rect,
    /// The comment composer, docked below the diff while one is open.
    pub composer: Option<Rect>,
    /// The submit form, docked in the same place.
    pub submit: Option<Rect>,
    /// The panel being read, while one is open. It floats over the panes
    /// rather than docking: it is read, not typed into, so nothing under it
    /// has to stay visible.
    pub overlay: Option<Overlay>,
    pub rows: Rows,
    pub files: Tree,
    /// The runs of the open file its patch left out, which the hunk headers
    /// standing for them are drawn from.
    pub gaps: Vec<Gap>,
}

impl Layout {
    pub fn compute(area: Rect, app: AppView<'_>) -> Self {
        let panes = split(
            area,
            Direction::Vertical,
            [
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ],
        );
        let (header, body, status) = (panes[0], panes[1], panes[2]);

        let (files_pane, diff_pane) = if app.is_files_visible {
            let columns = split(
                body,
                Direction::Horizontal,
                [
                    Constraint::Length(files_width(body.width)),
                    Constraint::Min(MIN_DIFF_WIDTH),
                ],
            );
            (Some(columns[0]), columns[1])
        } else {
            (None, body)
        };

        // Both panes carry their title on a top border, so a row of each goes
        // to chrome before any content is placed. The tree also carries the
        // rule between the two panes, and its list used to be measured wide
        // enough to paint over it.
        let files_inner = files_pane.map(|pane| Rect {
            width: pane.width.saturating_sub(1),
            ..inside(pane)
        });
        let has_prompt = app.file_filter.is_some();
        let files_prompt = files_inner
            .filter(|inner| has_prompt && inner.height > 0)
            .map(|inner| Rect { height: 1, ..inner });
        let files_list = files_inner.map(|inner| match files_prompt {
            Some(_) => inside(inner),
            None => inner,
        });
        let (diff, composer, submit) = dock(inside(diff_pane), app);
        let gaps = app.gaps();

        Self {
            header,
            body,
            status,
            files_pane,
            files_list,
            files_prompt,
            diff_pane,
            diff,
            composer,
            submit,
            overlay: build_overlay(app, body),
            rows: build_rows(app, diff, &gaps),
            gaps,
            files: build_tree(
                app,
                files_list.map_or(0, |list| list.height as usize),
            ),
        }
    }

    pub const fn diff_viewport(&self) -> usize {
        self.diff.height as usize
    }

    /// The row list the next frame will draw, for a keystroke that has moved
    /// the ground under this one. Opening another file or unfolding a card
    /// changes the list, and a scroll measured against the drawn one lands
    /// nowhere near what the reader asked for.
    pub fn rebuild_rows(&self, app: AppView<'_>) -> Rows {
        build_rows(app, self.diff, &app.gaps())
    }

    pub fn files_viewport(&self) -> usize {
        self.files_list.map_or(0, |list| list.height as usize)
    }

    /// The rows the diff keeps once an editor docks under it.
    ///
    /// Whatever opens an editor runs against the layout from before it existed,
    /// so it has to pull the cursor into what will be left rather than what is
    /// there now. Measured against the taller of the two editors, which is
    /// visible for either and needs no guess about which is opening.
    pub const fn viewport_once_docked(&self) -> usize {
        self.diff_viewport().saturating_sub(SUBMIT_HEIGHT as usize)
    }

    /// The rows the open panel can be scrolled by, which is however much of it
    /// does not fit.
    pub fn overlay_viewport(&self) -> usize {
        self.overlay
            .as_ref()
            .map_or(0, |overlay| overlay.inner.height as usize)
    }

    pub fn overlay_len(&self) -> usize {
        self.overlay
            .as_ref()
            .map_or(0, |overlay| overlay.content.len())
    }

    pub fn overlay_limit(&self) -> usize {
        self.overlay_len().saturating_sub(self.overlay_viewport())
    }
}

/// A panel floating over the panes, laid out: where it sits and what it says.
pub struct Overlay {
    /// The bordered box, for the frame to clear and paint.
    pub area: Rect,
    /// What is left inside the border, which is what scrolls.
    pub inner: Rect,
    pub title: &'static str,
    pub content: Content,
}

/// What an open panel is showing.
///
/// The reference stays descriptors, since its columns are budgeted against the
/// width as it is painted. The overview is prose, and prose wraps to a width,
/// which is settled here.
pub enum Content {
    Keys(Vec<Reference>),
    Overview(overview::Rows),
}

impl Content {
    const fn len(&self) -> usize {
        match self {
            Self::Keys(lines) => lines.len(),
            Self::Overview(rows) => rows.len(),
        }
    }

    /// One row as plain text, which is what a query is tested against. The
    /// reference's columns join into one line so a query can run over the chord
    /// and what it does at once.
    fn row_text(&self, index: usize) -> Option<String> {
        match self {
            Self::Keys(entries) => {
                entries.get(index).map(|entry| match entry {
                    Reference::Heading(title) => (*title).to_owned(),
                    Reference::Entry {
                        keys,
                        name,
                        summary,
                    } => format!("{keys} {name} {summary}"),
                })
            }
            Self::Overview(rows) => {
                rows.lines.get(index).map(ToString::to_string)
            }
        }
    }
}

impl Overlay {
    /// The rows a query hits, in reading order, which is what `n` steps
    /// through.
    pub fn matches(&self, query: Query<'_>) -> Vec<usize> {
        (0..self.content.len())
            .filter(|index| {
                self.content
                    .row_text(*index)
                    .is_some_and(|text| query.is_match(&text))
            })
            .collect()
    }
}

/// Widest a panel gets before it stops using the whole terminal.
const OVERLAY_WIDTH: u16 = 80;

/// Rows of chrome around a panel: two borders, and a row of margin above and
/// below so it reads as a panel rather than as a second pane.
const OVERLAY_MARGIN: u16 = 2;

pub(crate) fn panel_area(area: Rect) -> Rect {
    let width = OVERLAY_WIDTH.min(area.width);
    let height = area.height.saturating_sub(OVERLAY_MARGIN * 2).max(3);

    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

pub(crate) const fn panel_inner(area: Rect) -> Rect {
    Rect {
        x: area.x + 2,
        y: area.y + 2,
        width: area.width.saturating_sub(4),
        height: area.height.saturating_sub(4),
    }
}

fn build_overlay(app: AppView<'_>, body: Rect) -> Option<Overlay> {
    let mode = app.overlay_mode()?;

    let area = panel_area(body);
    let inner = panel_inner(area);

    let (title, content) = if mode == Mode::Help {
        (" keys ", Content::Keys(app.keymap().reference()))
    } else {
        (
            " overview ",
            Content::Overview(build_overview(app, inner.width)),
        )
    };

    Some(Overlay {
        area,
        inner,
        title,
        content,
    })
}

fn build_overview(app: AppView<'_>, width: u16) -> overview::Rows {
    match app.summary {
        SummaryState::Ready(summary) => overview::build(
            summary,
            app.pr.map_or("", |pr| pr.body.as_str()),
            app.discussion,
            app.overview_folds,
            width as usize,
            app.theme(),
        ),
        SummaryState::Failed(error) => overview::Rows {
            lines: vec![Line::styled(
                format!("error: {error}"),
                Style::default().fg(app.theme().danger),
            )],
            folds: vec![None],
        },
        SummaryState::Absent | SummaryState::Loading => overview::Rows {
            lines: vec![Line::from(vec![
                Span::styled(
                    SPINNER[app.loading_frame % SPINNER.len()],
                    Style::default().fg(app.theme().accent),
                ),
                Span::styled(
                    "  loading the overview",
                    Style::default().fg(app.theme().dim),
                ),
            ])],
            folds: vec![None],
        },
    }
}

/// Rows the composer takes, which is also the submit form's editor budget.
const COMPOSER_HEIGHT: u16 = 10;

/// The submit form adds a verdict row and the rule under it.
const SUBMIT_HEIGHT: u16 = COMPOSER_HEIGHT + 2;

/// Splits the diff so an open editor sits below it rather than over it.
///
/// Taking the rows instead of floating on them is what keeps the line being
/// commented on visible: the diff viewport shrinks, so the cursor scrolls into
/// what is left of it. Only one editor is ever open, and the submit form wins if
/// both somehow are.
fn dock(diff: Rect, app: AppView<'_>) -> (Rect, Option<Rect>, Option<Rect>) {
    let wanted = if app.submission.is_some() {
        SUBMIT_HEIGHT
    } else if app.composer.is_some() {
        COMPOSER_HEIGHT
    } else {
        return (diff, None, None);
    };

    let rows = split(
        diff,
        Direction::Vertical,
        [
            Constraint::Min(0),
            Constraint::Length(wanted.min(diff.height)),
        ],
    );

    if app.submission.is_some() {
        (rows[0], None, Some(rows[1]))
    } else {
        (rows[0], Some(rows[1]), None)
    }
}

/// The file tree, scrolled to wherever the cursor is resting.
fn build_tree(app: AppView<'_>, height: usize) -> Tree {
    let unresolved: Vec<usize> = app
        .files
        .iter()
        .map(|file| app.unresolved_threads(&file.path))
        .collect();

    let mut tree = Tree::build(
        app.files,
        &app.filtered_file_indices(),
        app.collapsed(),
        app.file_filter.is_some(),
        &unresolved,
    );

    let cursor = app.tree_directory().map_or_else(
        || tree.row_of(app.selected_file),
        |path| tree.row_of_directory(path),
    );
    tree.focus(cursor.unwrap_or(0), height);

    tree
}

fn build_rows(app: AppView<'_>, diff: Rect, gaps: &[Gap]) -> Rows {
    let Some(open) = app.open() else {
        return Rows::empty();
    };

    Rows::build(
        open.patch,
        open.threads,
        RowView {
            focused: app.focused_card,
            expanded: app.expanded_card,
            scroll: app.thread_scroll,
            width: diff.width as usize,
            window: rows::thread_window(diff.height as usize),
            theme: app.theme(),
            drafts: &open.drafts,
            gaps,
        },
    )
}

fn split<const N: usize>(
    area: Rect,
    direction: Direction,
    constraints: [Constraint; N],
) -> std::rc::Rc<[Rect]> {
    ratatui::layout::Layout::default()
        .direction(direction)
        .constraints(constraints)
        .split(area)
}

/// The area left after a pane's top border, or its prompt row.
const fn inside(area: Rect) -> Rect {
    Rect {
        y: area.y.saturating_add(1),
        height: area.height.saturating_sub(1),
        ..area
    }
}

/// Roughly a quarter of the terminal, clamped so the tree neither crowds the
/// diff on a narrow window nor sprawls on a wide one.
fn files_width(total: u16) -> u16 {
    (total / 4)
        .clamp(22, 34)
        .min(total.saturating_sub(MIN_DIFF_WIDTH))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;

    #[test]
    fn the_tree_never_starves_the_diff() {
        assert_eq!(files_width(120), 30);
        // Wide terminals cap the tree rather than letting it sprawl.
        assert_eq!(files_width(400), 34);
        // Narrow ones give the diff its floor first.
        assert_eq!(files_width(40), 20);
    }

    #[test]
    fn splits_a_frame_into_header_body_and_status() {
        let app = App::new();
        let layout = Layout::compute(Rect::new(0, 0, 120, 30), app.view());

        assert_eq!(layout.header, Rect::new(0, 0, 120, 1));
        assert_eq!(layout.status, Rect::new(0, 29, 120, 1));
        // Header, the diff pane's title rule, and the status bar.
        assert_eq!(layout.diff_viewport(), 27);
        assert_eq!(layout.files_pane.map(|pane| pane.width), Some(30));
        // No filter open, so the tree's whole inside scrolls.
        assert_eq!(layout.files_viewport(), 27);
        assert!(layout.files_prompt.is_none());
    }
}
