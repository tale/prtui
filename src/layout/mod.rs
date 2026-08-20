pub mod rows;
pub mod tree;

use crate::app::App;
use ratatui::layout::{Constraint, Direction, Rect};
use rows::{Rows, View};
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
    pub rows: Rows,
    pub files: Tree,
}

impl Layout {
    pub fn compute(area: Rect, app: &App) -> Self {
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
            rows: build_rows(app, diff),
            files: build_tree(
                app,
                files_list.map_or(0, |list| list.height as usize),
            ),
        }
    }

    pub const fn diff_viewport(&self) -> usize {
        self.diff.height as usize
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
fn dock(diff: Rect, app: &App) -> (Rect, Option<Rect>, Option<Rect>) {
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
fn build_tree(app: &App, height: usize) -> Tree {
    let unresolved: Vec<usize> = app
        .files
        .iter()
        .map(|file| app.unresolved_threads(&file.path))
        .collect();

    let mut tree = Tree::build(
        &app.files,
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

fn build_rows(app: &App, diff: Rect) -> Rows {
    let Some(open) = app.open() else {
        return Rows::empty();
    };

    Rows::build(
        open.patch,
        open.threads,
        View {
            focused: app.focused_thread.as_deref(),
            expanded: app.expanded_thread.as_deref(),
            scroll: app.thread_scroll,
            width: diff.width as usize,
            window: rows::thread_window(diff.height as usize),
            theme: app.theme(),
            drafts: &open.drafts,
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
        let layout = Layout::compute(Rect::new(0, 0, 120, 30), &App::new());

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
