pub mod action;
pub mod draft;
pub mod editor;
pub mod input;
pub mod keymap;
pub mod mode;

use crate::model::{ChangedFile, PullRequest, ReviewThread};
use crate::renderer::{Renderer, Segment, Theme, ThemeMode};
use action::{Action, Motion};
use draft::{Anchor, Draft};
use editor::CommentEditor;
use mode::{Mode, Selection};
use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Pane {
    Files,
    Diff,
}

/// An in-progress comment: the editor buffer plus where it will land.
pub struct Composer {
    pub editor: CommentEditor,
    pub anchor: Anchor,
    pub path: String,
}

struct FileFilterSnapshot {
    query: Option<String>,
    selected_file: usize,
}

pub struct App {
    pub pr: Option<PullRequest>,
    pub files: Vec<ChangedFile>,
    pub threads_by_path: HashMap<String, Vec<ReviewThread>>,
    pub drafts: Vec<Draft>,

    pub mode: Mode,
    pub selection: Option<Selection>,
    pub composer: Option<Composer>,
    pub file_filter: Option<CommentEditor>,
    pub selected_file: usize,
    pub cursor: usize,
    pub focused_thread: Option<String>,
    pub expanded_thread: Option<String>,
    pub diff_scroll: usize,
    pub pane: Pane,
    pub is_files_visible: bool,

    pub status: String,
    pub loading_frame: usize,
    pub should_quit: bool,

    renderer: Renderer,
    highlights: HashMap<usize, Vec<Vec<Segment>>>,
    filter_snapshot: Option<FileFilterSnapshot>,
    files_loaded: bool,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_renderer(Renderer::default())
    }

    pub fn with_renderer(renderer: Renderer) -> Self {
        Self {
            pr: None,
            files: Vec::new(),
            threads_by_path: HashMap::new(),
            drafts: Vec::new(),
            mode: Mode::Normal,
            selection: None,
            composer: None,
            file_filter: None,
            selected_file: 0,
            cursor: 0,
            focused_thread: None,
            expanded_thread: None,
            diff_scroll: 0,
            pane: Pane::Files,
            is_files_visible: true,
            status: String::new(),
            loading_frame: 0,
            should_quit: false,
            renderer,
            highlights: HashMap::new(),
            filter_snapshot: None,
            files_loaded: false,
        }
    }

    pub const fn theme(&self) -> Theme {
        self.renderer.theme()
    }

    pub const fn is_loading(&self) -> bool {
        !self.files_loaded
    }

    pub fn advance_loading(&mut self) {
        self.loading_frame = self.loading_frame.wrapping_add(1);
    }

    /// File patches are the only data required to make the main review surface
    /// useful. PR metadata and review threads may arrive independently later.
    pub fn set_files(&mut self, files: Vec<ChangedFile>) {
        self.files = files;
        self.files_loaded = true;
    }

    /// Swap the complete renderer palette and discard syntax colors produced
    /// under the previous terminal appearance.
    pub fn set_theme_mode(&mut self, mode: ThemeMode) -> bool {
        if self.renderer.theme().mode == mode {
            return false;
        }

        self.renderer = Renderer::new(mode);
        self.highlights.clear();
        true
    }

    pub fn set_meta(&mut self, pr: PullRequest) {
        let mut by_path: HashMap<String, Vec<ReviewThread>> = HashMap::new();
        for thread in &pr.threads {
            by_path
                .entry(thread.path.clone())
                .or_default()
                .push(thread.clone());
        }

        self.threads_by_path = by_path;
        self.pr = Some(pr);
    }

    pub fn current_file(&self) -> Option<&ChangedFile> {
        self.files.get(self.selected_file)
    }

    pub fn diff_len(&self) -> usize {
        self.current_file().map(|f| f.lines.len()).unwrap_or(0)
    }

    pub fn ensure_highlighted(&mut self) {
        let index = self.selected_file;
        if self.highlights.contains_key(&index) {
            return;
        }

        let Some(file) = self.files.get(index) else {
            return;
        };

        let styled = self.renderer.highlight_file(&file.path, &file.lines);
        self.highlights.insert(index, styled);
    }

    /// Never clobbers a file that was already highlighted on demand.
    pub fn set_highlight(&mut self, index: usize, styled: Vec<Vec<Segment>>) {
        self.highlights.entry(index).or_insert(styled);
    }

    pub fn highlighted(&self) -> Option<&[Vec<Segment>]> {
        self.highlights
            .get(&self.selected_file)
            .map(|v| v.as_slice())
    }

    pub fn apply(&mut self, action: Action, viewport_height: usize) {
        match action {
            Action::Quit => self.should_quit = true,
            Action::TogglePane => self.toggle_pane(),
            Action::ToggleTree => {
                self.is_files_visible = !self.is_files_visible;
                if self.is_files_visible {
                    self.pane = Pane::Files;
                } else {
                    self.pane = Pane::Diff;
                }
            }
            Action::Activate => self.activate(),
            Action::LeaveThread => self.set_focused_thread(None),
            Action::FocusFiles => self.focus_files(),
            Action::FocusDiff => self.focus_diff(),
            Action::StartFileFilter => self.start_file_filter(),
            Action::AcceptFileFilter => self.accept_file_filter(),
            Action::CancelFileFilter => self.cancel_file_filter(),
            Action::ClearFileFilter => self.clear_file_filter(),
            Action::NextFile => self.step_file(1),
            Action::PrevFile => self.step_file(-1),
            Action::Move(motion) => self.travel(motion, viewport_height),

            Action::EnterVisual => {
                if self.pane == Pane::Diff {
                    self.set_focused_thread(None);
                    self.mode = Mode::Visual;
                    self.selection = Some(Selection::at(self.cursor));
                }
            }
            Action::LeaveVisual => {
                self.mode = Mode::Normal;
                self.selection = None;
            }

            Action::StartComment => self.start_comment(),
            Action::CommitComment => self.commit_comment(),
            Action::CancelComment => {
                self.composer = None;
                self.mode = Mode::Normal;
                self.selection = None;
            }
        }
    }

    fn start_comment(&mut self) {
        if self.pane != Pane::Diff {
            return;
        }

        if self.focused_thread.is_some() {
            self.status = "thread replies are not available yet".into();
            return;
        }

        let rows = match self.selection {
            Some(selection) => selection.range(),
            None => self.cursor..=self.cursor,
        };

        let Some(file) = self.current_file() else {
            return;
        };
        let Some(anchor) = draft::anchor_for(file, rows) else {
            self.status = "cannot comment on that line".into();
            return;
        };

        self.composer = Some(Composer {
            editor: CommentEditor::default(),
            anchor,
            path: file.path.clone(),
        });
        self.mode = Mode::Insert;
    }

    fn start_file_filter(&mut self) {
        self.is_files_visible = true;
        self.pane = Pane::Files;
        let query = self.filter_query();
        self.filter_snapshot = Some(FileFilterSnapshot {
            query: query.clone(),
            selected_file: self.selected_file,
        });
        match query {
            Some(query) => self.file_filter.get_or_insert_default().set_text(query),
            None => self.file_filter = Some(CommentEditor::default()),
        }
        self.mode = Mode::Filter;
    }

    fn accept_file_filter(&mut self) {
        if self.mode != Mode::Filter || self.filtered_file_indices().is_empty() {
            return;
        }

        self.filter_snapshot = None;
        if self.filter_query().is_some_and(|query| query.is_empty()) {
            self.file_filter = None;
        }
        self.mode = Mode::Normal;
        self.pane = Pane::Files;
    }

    fn cancel_file_filter(&mut self) {
        let snapshot = self.filter_snapshot.take();
        self.file_filter = snapshot
            .as_ref()
            .and_then(|snapshot| snapshot.query.as_ref())
            .map(|query| {
                let mut editor = CommentEditor::default();
                editor.set_text(query);
                editor
            });
        if let Some(snapshot) = snapshot {
            self.set_selected_file(snapshot.selected_file, false);
        }
        self.mode = Mode::Normal;
        self.pane = Pane::Files;
    }

    fn clear_file_filter(&mut self) {
        self.file_filter = None;
        self.filter_snapshot = None;
    }

    pub fn filter_query(&self) -> Option<String> {
        self.file_filter.as_ref().map(CommentEditor::text)
    }

    pub fn filtered_file_indices(&self) -> Vec<usize> {
        let Some(query) = self.filter_query() else {
            return (0..self.files.len()).collect();
        };
        let query = query.to_lowercase();

        self.files
            .iter()
            .enumerate()
            .filter_map(|(index, file)| file.path.to_lowercase().contains(&query).then_some(index))
            .collect()
    }

    /// Keep the current file when it still matches; otherwise preview the
    /// first result as the query changes.
    pub fn sync_file_filter(&mut self) {
        let matches = self.filtered_file_indices();
        if matches.is_empty() || matches.contains(&self.selected_file) {
            return;
        }

        self.set_selected_file(matches[0], false);
    }

    fn commit_comment(&mut self) {
        let Some(composer) = self.composer.take() else {
            return;
        };

        let body = composer.editor.text();
        let body = body.trim().to_string();

        self.mode = Mode::Normal;
        self.selection = None;

        if body.is_empty() {
            self.status = "empty comment discarded".into();
            return;
        }

        self.drafts.push(Draft {
            path: composer.path,
            start_line: composer.anchor.start_line,
            end_line: composer.anchor.end_line,
            side: composer.anchor.side,
            body,
        });

        self.status = "draft saved".into();
    }

    fn toggle_pane(&mut self) {
        if self.mode != Mode::Normal {
            return;
        }

        if self.pane == Pane::Files {
            self.focus_diff();
        } else {
            self.focus_files();
        }
    }

    fn activate(&mut self) {
        if self.pane == Pane::Files {
            self.focus_diff();
            return;
        }

        let Some(id) = self.focused_thread.clone() else {
            return;
        };
        self.expanded_thread = (self.expanded_thread.as_deref() != Some(&id)).then_some(id);
    }

    pub fn is_thread_expanded(&self, id: &str) -> bool {
        self.expanded_thread.as_deref() == Some(id)
    }

    fn focus_files(&mut self) {
        if self.mode != Mode::Normal {
            return;
        }

        self.is_files_visible = true;
        self.pane = Pane::Files;
    }

    fn focus_diff(&mut self) {
        if self.mode == Mode::Normal {
            self.pane = Pane::Diff;
        }
    }

    fn select_file(&mut self, index: usize) {
        self.set_selected_file(index, true);
    }

    fn step_file(&mut self, direction: isize) {
        if self.file_filter.is_none() {
            let target = self.selected_file.saturating_add_signed(direction);
            self.select_file(target);
            return;
        }

        let matches = self.filtered_file_indices();
        let Some(position) = matches
            .iter()
            .position(|&index| index == self.selected_file)
        else {
            return;
        };
        let target = position
            .saturating_add_signed(direction)
            .min(matches.len().saturating_sub(1));
        self.select_file(matches[target]);
    }

    fn set_selected_file(&mut self, index: usize, leave_transient_mode: bool) {
        if self.files.is_empty() {
            return;
        }

        self.selected_file = index.min(self.files.len() - 1);
        self.cursor = 0;
        self.set_focused_thread(None);
        self.diff_scroll = 0;
        self.selection = None;
        if leave_transient_mode {
            self.mode = Mode::Normal;
        }
    }

    fn travel(&mut self, motion: Motion, viewport_height: usize) {
        if self.pane == Pane::Files {
            if self.file_filter.is_some() {
                let matches = self.filtered_file_indices();
                let Some(position) = matches
                    .iter()
                    .position(|&index| index == self.selected_file)
                else {
                    return;
                };
                let target = match motion {
                    Motion::Down(n) => position.saturating_add(n),
                    Motion::Up(n) => position.saturating_sub(n),
                    Motion::HalfPageDown => position.saturating_add(viewport_height / 2),
                    Motion::HalfPageUp => position.saturating_sub(viewport_height / 2),
                    Motion::Top => 0,
                    Motion::Bottom => matches.len().saturating_sub(1),
                }
                .min(matches.len().saturating_sub(1));
                self.set_selected_file(matches[target], false);
                return;
            }

            let target = match motion {
                Motion::Down(n) => self.selected_file.saturating_add(n),
                Motion::Up(n) => self.selected_file.saturating_sub(n),
                Motion::HalfPageDown => self.selected_file.saturating_add(viewport_height / 2),
                Motion::HalfPageUp => self.selected_file.saturating_sub(viewport_height / 2),
                Motion::Top => 0,
                Motion::Bottom => self.files.len().saturating_sub(1),
            };

            self.select_file(target);
            return;
        }

        let last = self.diff_len().saturating_sub(1);
        if self.mode == Mode::Visual {
            self.cursor = match motion {
                Motion::Down(n) => self.cursor.saturating_add(n).min(last),
                Motion::Up(n) => self.cursor.saturating_sub(n),
                Motion::HalfPageDown => self.cursor.saturating_add(viewport_height / 2).min(last),
                Motion::HalfPageUp => self.cursor.saturating_sub(viewport_height / 2),
                Motion::Top => 0,
                Motion::Bottom => last,
            };
        } else {
            match motion {
                Motion::Down(n) => self.move_diff_stops(1, n),
                Motion::Up(n) => self.move_diff_stops(-1, n),
                Motion::HalfPageDown => self.move_diff_stops(1, viewport_height / 2),
                Motion::HalfPageUp => self.move_diff_stops(-1, viewport_height / 2),
                Motion::Top => {
                    self.cursor = 0;
                    self.set_focused_thread(None);
                }
                Motion::Bottom => {
                    self.cursor = last;
                    self.set_focused_thread(None);
                }
            }
        }

        if let Some(selection) = &mut self.selection {
            selection.head = self.cursor;
        }

        self.follow_cursor(viewport_height);
    }

    fn move_diff_stops(&mut self, direction: isize, count: usize) {
        let max_steps = self.diff_len().saturating_add(
            self.current_file()
                .and_then(|file| self.threads_by_path.get(&file.path))
                .map_or(0, Vec::len),
        );

        for _ in 0..count.min(max_steps) {
            if !self.move_diff_stop(direction) {
                break;
            }
        }
    }

    fn move_diff_stop(&mut self, direction: isize) -> bool {
        let ids = self.thread_ids_at_row(self.cursor);

        if direction > 0 {
            if let Some(focused) = self.focused_thread.as_deref() {
                if let Some(position) = ids.iter().position(|id| id == focused)
                    && let Some(next) = ids.get(position + 1)
                {
                    self.set_focused_thread(Some(next.clone()));
                    return true;
                }
                if self.cursor + 1 < self.diff_len() {
                    self.cursor += 1;
                    self.set_focused_thread(None);
                    return true;
                }
                return false;
            }

            if let Some(first) = ids.first() {
                self.set_focused_thread(Some(first.clone()));
                return true;
            }
            if self.cursor + 1 < self.diff_len() {
                self.cursor += 1;
                return true;
            }
            return false;
        }

        if let Some(focused) = self.focused_thread.as_deref() {
            if let Some(position) = ids.iter().position(|id| id == focused) {
                if position > 0 {
                    self.set_focused_thread(Some(ids[position - 1].clone()));
                } else {
                    self.set_focused_thread(None);
                }
                return true;
            }
            self.set_focused_thread(None);
            return true;
        }

        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        let previous = self.thread_ids_at_row(self.cursor);
        self.set_focused_thread(previous.last().cloned());
        true
    }

    fn set_focused_thread(&mut self, focused: Option<String>) {
        if self.focused_thread != focused {
            self.expanded_thread = None;
        }
        self.focused_thread = focused;
    }

    fn thread_ids_at_row(&self, row: usize) -> Vec<String> {
        let Some(file) = self.current_file() else {
            return Vec::new();
        };
        let Some(line) = file.lines.get(row) else {
            return Vec::new();
        };
        let is_last = row + 1 == file.lines.len();
        let mut threads: Vec<&ReviewThread> = self
            .threads_by_path
            .get(&file.path)
            .into_iter()
            .flatten()
            .filter(|thread| {
                (thread.is_outdated && is_last) || (!thread.is_outdated && thread.anchors_to(line))
            })
            .collect();
        threads.sort_by_key(|thread| {
            if thread.is_outdated {
                2
            } else if thread.is_resolved {
                1
            } else {
                0
            }
        });
        threads.iter().map(|thread| thread.id.clone()).collect()
    }

    /// Keeps the cursor inside the viewport with a small scroll-off margin.
    fn follow_cursor(&mut self, viewport_height: usize) {
        if viewport_height == 0 {
            return;
        }

        let margin = 3.min(viewport_height / 4);

        if self.cursor < self.diff_scroll + margin {
            self.diff_scroll = self.cursor.saturating_sub(margin);
            return;
        }

        let bottom = self.diff_scroll + viewport_height.saturating_sub(margin + 1);
        if self.cursor > bottom {
            self.diff_scroll = (self.cursor + margin + 1).saturating_sub(viewport_height);
        }
    }
}
