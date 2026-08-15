pub mod action;
pub mod draft;
pub mod editor;
pub mod input;
pub mod keymap;
pub mod mode;
pub mod search;

use crate::images::Images;
use crate::model::{ChangedFile, PullRequest, ReviewThread};
use crate::renderer::{Renderer, Segment, Theme, ThemeMode, markdown};
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

/// Where the diff sat when a search began, so cancelling undoes the incremental
/// preview instead of stranding the cursor on a match the user rejected.
struct SearchOrigin {
    query: Option<String>,
    cursor: usize,
    focused_thread: Option<String>,
    diff_scroll: usize,
}

pub struct App {
    pub pr: Option<PullRequest>,
    pub files: Vec<ChangedFile>,
    pub threads_by_path: HashMap<String, Vec<ReviewThread>>,
    pub drafts: Vec<Draft>,
    pub images: Images,

    pub mode: Mode,
    pub selection: Option<Selection>,
    pub composer: Option<Composer>,
    pub file_filter: Option<CommentEditor>,
    pub search: Option<CommentEditor>,
    pub selected_file: usize,
    pub cursor: usize,
    pub focused_thread: Option<String>,
    pub expanded_thread: Option<String>,
    pub thread_scroll: usize,
    pub thread_scroll_limit: usize,
    pub diff_scroll: usize,
    pub pane: Pane,
    pub is_files_visible: bool,

    pub status: String,
    pub loading_frame: usize,
    pub should_quit: bool,

    renderer: Renderer,
    highlights: HashMap<usize, Vec<Vec<Segment>>>,
    filter_snapshot: Option<FileFilterSnapshot>,
    search_origin: Option<SearchOrigin>,
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
            images: Images::default(),
            mode: Mode::Normal,
            selection: None,
            composer: None,
            file_filter: None,
            search: None,
            selected_file: 0,
            cursor: 0,
            focused_thread: None,
            expanded_thread: None,
            thread_scroll: 0,
            thread_scroll_limit: 0,
            diff_scroll: 0,
            pane: Pane::Files,
            is_files_visible: true,
            status: String::new(),
            loading_frame: 0,
            should_quit: false,
            renderer,
            highlights: HashMap::new(),
            filter_snapshot: None,
            search_origin: None,
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
            Action::StartFind => self.start_find(),
            Action::ClearFind => self.clear_find(),
            Action::AcceptFileFilter => self.accept_file_filter(),
            Action::CancelFileFilter => self.cancel_file_filter(),
            Action::AcceptSearch => self.accept_search(),
            Action::CancelSearch => self.cancel_search(),
            Action::NextMatch => self.jump_match(1, viewport_height),
            Action::PrevMatch => self.jump_match(-1, viewport_height),
            Action::NextFile => self.step_file(1),
            Action::PrevFile => self.step_file(-1),
            Action::NextComment => self.jump_comment(1, viewport_height),
            Action::PrevComment => self.jump_comment(-1, viewport_height),
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

    /// `/` means "narrow what I am looking at", which is the file list from the
    /// tree and the open patch from the diff.
    fn start_find(&mut self) {
        if self.pane == Pane::Files {
            self.start_file_filter();
        } else {
            self.start_search();
        }
    }

    fn clear_find(&mut self) {
        if self.pane == Pane::Diff && self.search.is_some() {
            self.search = None;
            self.search_origin = None;
            return;
        }

        self.file_filter = None;
        self.filter_snapshot = None;
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

    /// The query starts empty rather than prefilled: the previous pattern is
    /// still what `n` repeats, and cancelling puts it back.
    fn start_search(&mut self) {
        self.search_origin = Some(SearchOrigin {
            query: self.search_query(),
            cursor: self.cursor,
            focused_thread: self.focused_thread.clone(),
            diff_scroll: self.diff_scroll,
        });
        self.search = Some(CommentEditor::default());
        self.pane = Pane::Diff;
        self.mode = Mode::Search;
        self.selection = None;
    }

    fn accept_search(&mut self) {
        if self.mode != Mode::Search {
            return;
        }

        self.search_origin = None;
        self.mode = Mode::Normal;

        let Some(query) = self.search_query().filter(|query| !query.is_empty()) else {
            self.search = None;
            return;
        };

        if self.search_matches().is_empty() {
            self.status = format!("pattern not found: {query}");
        }
    }

    fn cancel_search(&mut self) {
        self.mode = Mode::Normal;

        let Some(origin) = self.search_origin.take() else {
            return;
        };

        self.search = origin.query.map(|query| {
            let mut editor = CommentEditor::default();
            editor.set_text(&query);
            editor
        });
        self.cursor = origin.cursor;
        self.diff_scroll = origin.diff_scroll;
        self.set_focused_thread(origin.focused_thread);
    }

    /// Incremental search: every keystroke previews the first match from where
    /// the search began, so the diff tracks the query as it is typed.
    pub fn sync_search(&mut self, viewport_height: usize) {
        let Some(origin) = self.search_origin.as_ref() else {
            return;
        };

        self.cursor = origin.cursor;
        self.diff_scroll = origin.diff_scroll;

        let matches = self.search_matches();
        let Some(hit) = matches
            .iter()
            .find(|hit| hit.row() >= self.cursor)
            .or_else(|| matches.first())
            .cloned()
        else {
            return;
        };

        self.land_on(
            hit.row(),
            hit.thread_id().map(str::to_string),
            viewport_height,
        );
    }

    pub fn search_query(&self) -> Option<String> {
        self.search.as_ref().map(CommentEditor::text)
    }

    /// Every hit in the open file, ordered the way the diff renders them: a code
    /// line, then the threads that hang beneath it.
    pub fn search_matches(&self) -> Vec<search::Match> {
        let Some(query) = self.search_query().filter(|query| !query.is_empty()) else {
            return Vec::new();
        };
        let Some(file) = self.current_file() else {
            return Vec::new();
        };

        let hits: Vec<(usize, String)> = self
            .thread_rows(self.selected_file)
            .into_iter()
            .filter(|(_, thread)| {
                thread.comments.iter().any(|comment| {
                    search::is_match(&comment.body, &query)
                        || search::is_match(&comment.author, &query)
                })
            })
            .map(|(row, thread)| (row, thread.id.clone()))
            .collect();

        let mut matches = Vec::new();
        for (row, line) in file.lines.iter().enumerate() {
            if search::is_match(&line.text, &query) {
                matches.push(search::Match::Line(row));
            }

            matches.extend(hits.iter().filter(|(hit, _)| *hit == row).map(|(_, id)| {
                search::Match::Thread {
                    row,
                    id: id.clone(),
                }
            }));
        }

        matches
    }

    /// One-based cursor position within the match list, plus the total. A zero
    /// position means the cursor is currently between matches.
    pub fn search_summary(&self) -> (usize, usize) {
        let matches = self.search_matches();
        let current = self.match_position(&matches).map_or(0, |index| index + 1);

        (current, matches.len())
    }

    /// Byte ranges to paint on one diff row, for the renderer.
    pub fn line_match_ranges(&self, row: usize) -> Vec<std::ops::Range<usize>> {
        let Some(query) = self.search_query().filter(|query| !query.is_empty()) else {
            return Vec::new();
        };
        let Some(line) = self.current_file().and_then(|file| file.lines.get(row)) else {
            return Vec::new();
        };

        search::ranges(&line.text, &query)
    }

    fn match_position(&self, matches: &[search::Match]) -> Option<usize> {
        matches.iter().position(|hit| {
            hit.row() == self.cursor && hit.thread_id() == self.focused_thread.as_deref()
        })
    }

    fn jump_match(&mut self, direction: isize, viewport_height: usize) {
        let Some(query) = self.search_query().filter(|query| !query.is_empty()) else {
            self.status = "no search pattern".into();
            return;
        };

        let matches = self.search_matches();
        if matches.is_empty() {
            self.status = format!("pattern not found: {query}");
            return;
        }

        // Searching is file-local, so both ends wrap rather than spilling into
        // the next file the way comment jumps do.
        let target = match self.match_position(&matches) {
            Some(index) if direction > 0 => (index + 1) % matches.len(),
            Some(index) => (index + matches.len() - 1) % matches.len(),
            None if direction > 0 => matches
                .iter()
                .position(|hit| hit.row() >= self.cursor)
                .unwrap_or(0),
            None => matches
                .iter()
                .rposition(|hit| hit.row() <= self.cursor)
                .unwrap_or(matches.len() - 1),
        };

        let hit = matches[target].clone();
        self.land_on(
            hit.row(),
            hit.thread_id().map(str::to_string),
            viewport_height,
        );
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
        self.thread_scroll = 0;
        self.thread_scroll_limit = 0;

        if let Some(expanded) = self.expanded_thread.clone() {
            self.request_thread_images(&expanded);
        }
    }

    /// Comment images are only worth fetching once their thread is opened.
    fn request_thread_images(&mut self, id: &str) {
        if !self.images.is_supported() {
            return;
        }

        let urls: Vec<String> = self
            .threads_by_path
            .values()
            .flatten()
            .find(|thread| thread.id == id)
            .into_iter()
            .flat_map(|thread| &thread.comments)
            .flat_map(|comment| markdown::image_urls(&comment.body))
            .collect();

        for url in urls {
            self.images.request(&url);
        }
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
        } else if self.expanded_thread.is_some() {
            self.thread_scroll = match motion {
                Motion::Down(n) => self.thread_scroll.saturating_add(n),
                Motion::Up(n) => self.thread_scroll.saturating_sub(n),
                Motion::HalfPageDown => self.thread_scroll.saturating_add(viewport_height / 2),
                Motion::HalfPageUp => self.thread_scroll.saturating_sub(viewport_height / 2),
                Motion::Top => 0,
                Motion::Bottom => self.thread_scroll_limit,
            }
            .min(self.thread_scroll_limit);
            return;
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

    fn jump_comment(&mut self, direction: isize, viewport_height: usize) {
        if let Some((row, id)) = self.comment_stop_here(direction) {
            self.land_on(row, Some(id), viewport_height);
            return;
        }

        let Some((index, row, id)) = self.comment_stop_elsewhere(direction) else {
            self.status = "no more comments".into();
            return;
        };

        self.set_selected_file(index, false);
        self.land_on(row, Some(id), viewport_height);
    }

    /// Puts the cursor on a diff row, optionally focusing one of its threads,
    /// and scrolls the row into view.
    fn land_on(&mut self, row: usize, thread: Option<String>, viewport_height: usize) {
        self.pane = Pane::Diff;
        self.selection = None;
        self.cursor = row;
        self.set_focused_thread(thread);
        self.follow_cursor(viewport_height);
        self.status.clear();
    }

    /// Every thread in a file paired with the diff row it renders under, in the
    /// order the cursor visits them. Outdated threads have no live anchor and
    /// are pinned after the last row, matching how they are drawn.
    fn thread_rows(&self, index: usize) -> Vec<(usize, &ReviewThread)> {
        let Some(file) = self.files.get(index) else {
            return Vec::new();
        };
        let Some(threads) = self.threads_by_path.get(&file.path) else {
            return Vec::new();
        };
        let last = file.lines.len().saturating_sub(1);

        let mut rows: Vec<(usize, u8, &ReviewThread)> = threads
            .iter()
            .filter_map(|thread| {
                if thread.is_outdated {
                    return Some((last, 2, thread));
                }

                let row = file.lines.iter().position(|line| thread.anchors_to(line))?;
                Some((row, u8::from(thread.is_resolved), thread))
            })
            .collect();

        rows.sort_by_key(|(row, rank, _)| (*row, *rank));
        rows.into_iter()
            .map(|(row, _, thread)| (row, thread))
            .collect()
    }

    /// The subset of threads that `}` and `{` stop at.
    fn comment_stops(&self, index: usize) -> Vec<(usize, String)> {
        self.thread_rows(index)
            .into_iter()
            .filter(|(_, thread)| !thread.is_resolved && !thread.is_outdated)
            .map(|(row, thread)| (row, thread.id.clone()))
            .collect()
    }

    /// A focused thread that is not itself a stop (resolved or outdated) falls
    /// back to the cursor row, so the jump still moves in the right direction.
    fn comment_stop_here(&self, direction: isize) -> Option<(usize, String)> {
        let stops = self.comment_stops(self.selected_file);
        let current = self
            .focused_thread
            .as_deref()
            .and_then(|focused| stops.iter().position(|(_, id)| id == focused));

        let target = match (current, direction > 0) {
            (Some(index), true) => (index + 1 < stops.len()).then(|| index + 1),
            (Some(index), false) => index.checked_sub(1),
            (None, true) => stops.iter().position(|(row, _)| *row >= self.cursor),
            (None, false) => stops.iter().rposition(|(row, _)| *row <= self.cursor),
        }?;

        Some(stops[target].clone())
    }

    fn comment_stop_elsewhere(&self, direction: isize) -> Option<(usize, usize, String)> {
        let visible = self.filtered_file_indices();
        let position = visible.iter().position(|&i| i == self.selected_file)?;

        let ahead: Vec<usize> = if direction > 0 {
            visible[position + 1..].to_vec()
        } else {
            visible[..position].iter().rev().copied().collect()
        };

        ahead.into_iter().find_map(|index| {
            let stops = self.comment_stops(index);
            let stop = if direction > 0 {
                stops.first()
            } else {
                stops.last()
            }?;

            Some((index, stop.0, stop.1.clone()))
        })
    }

    fn set_focused_thread(&mut self, focused: Option<String>) {
        if self.focused_thread != focused {
            self.expanded_thread = None;
            self.thread_scroll = 0;
            self.thread_scroll_limit = 0;
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
