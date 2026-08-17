//! Where everything goes this frame.
//!
//! Layout is a value, computed once from app state and handed to both the
//! renderer and the action handlers. Nothing here mutates the app: facts the
//! renderer would otherwise have to discover and write back — how far a
//! conversation can scroll, how tall a viewport is, which threads hang under
//! which line — are computed here and read from here instead.

pub mod measure;
pub mod rows;

use crate::app::App;
use ratatui::layout::{Constraint, Direction, Rect};
use rows::{Rows, View};

/// Narrowest the diff is allowed to get before the tree stops taking room.
const MIN_DIFF_WIDTH: u16 = 20;

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
    pub rows: Rows,
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
        // to chrome before any content is placed.
        let files_inner = files_pane.map(inside);
        let has_prompt = app.file_filter.is_some();
        let files_prompt = files_inner
            .filter(|inner| has_prompt && inner.height > 0)
            .map(|inner| Rect { height: 1, ..inner });
        let files_list = files_inner.map(|inner| match files_prompt {
            Some(_) => inside(inner),
            None => inner,
        });
        let diff = inside(diff_pane);

        Self {
            header,
            body,
            status,
            files_pane,
            files_list,
            files_prompt,
            diff_pane,
            diff,
            rows: build_rows(app, diff),
        }
    }

    pub const fn diff_viewport(&self) -> usize {
        self.diff.height as usize
    }

    pub fn files_viewport(&self) -> usize {
        self.files_list.map_or(0, |list| list.height as usize)
    }
}

fn build_rows(app: &App, diff: Rect) -> Rows {
    let Some(file) = app.current_file() else {
        return Rows::empty();
    };

    let threads = app
        .threads_by_path
        .get(&file.path)
        .map(Vec::as_slice)
        .unwrap_or_default();

    Rows::build(
        file,
        threads,
        View {
            focused: app.focused_thread.as_deref(),
            expanded: app.expanded_thread.as_deref(),
            scroll: app.thread_scroll,
            width: diff.width as usize,
            window: rows::thread_window(diff.height as usize),
            theme: app.theme(),
            images: &app.images,
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
