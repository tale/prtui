pub mod action;
pub mod draft;
pub mod editor;
pub mod input;
pub mod keymap;
pub mod mode;
pub mod review;
pub mod search;

use crate::layout::Layout;
use crate::layout::rows;
use crate::model::{ChangedFile, DiffLine, Meta, PullRequest, ReviewThread};
use crate::renderer::{Segment, Theme, ThemeMode};
use action::{Action, Motion};
use draft::{Anchor, Attachment, Draft};
use editor::CommentEditor;
use mode::{Mode, Selection};
use review::{Failure, Request, Sent, Submission};
use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;

/// Syntax colors for one file: the segments of each of its lines.
pub type Highlight = Vec<Vec<Segment>>;

/// The file under review, gathered from the four places its parts live.
///
/// The patch comes from REST, the conversation from GraphQL, the colors from
/// the highlighting thread, and the drafts from this session. Derived per
/// frame rather than stored, so nothing has to be kept in step with anything.
pub struct OpenFile<'a> {
    pub patch: &'a ChangedFile,
    pub threads: &'a [ReviewThread],
    pub drafts: Vec<&'a Draft>,
    highlight: Option<&'a Highlight>,
}

impl<'a> OpenFile<'a> {
    pub fn line(&self, index: usize) -> Option<&'a DiffLine> {
        self.patch.lines.get(index)
    }

    /// Colors for one line, absent until the background pass reaches the file.
    pub fn segments(&self, index: usize) -> Option<&'a [Segment]> {
        self.highlight?.get(index).map(Vec::as_slice)
    }

    /// The remark on the file as a whole, if one is pending.
    pub fn file_draft(&self) -> Option<&'a str> {
        self.drafts
            .iter()
            .find(|draft| draft.is_file_level())
            .map(|draft| draft.body.as_str())
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Pane {
    Files,
    Diff,
}

/// Where the reader is.
///
/// This is what a diff row needs to know that is not a property of the row
/// itself, gathered once so the renderers take a value instead of reaching
/// into the app for whatever they like.
#[derive(Clone, Copy)]
pub struct Focus<'a> {
    pub cursor: usize,
    pub selection: Option<Selection>,
    pub pane: Pane,
    pub thread: Option<&'a str>,
    pub expanded: Option<&'a str>,
    pub query: Option<&'a str>,
}

impl Focus<'_> {
    /// The diff cursor gives way while a thread holds the focus.
    pub fn is_cursor(&self, row: usize) -> bool {
        self.pane == Pane::Diff && self.thread.is_none() && row == self.cursor
    }

    pub fn is_selected(&self, row: usize) -> bool {
        self.selection
            .is_some_and(|selection| selection.contains(row))
    }

    pub fn is_thread_focused(&self, id: &str) -> bool {
        self.thread == Some(id)
    }

    pub fn is_thread_expanded(&self, id: &str) -> bool {
        self.expanded == Some(id)
    }

    /// Byte ranges of the active query within one line of the diff.
    pub fn matches(&self, text: &str) -> Vec<Range<usize>> {
        self.query
            .filter(|query| !query.is_empty())
            .map_or_else(Vec::new, |query| search::ranges(text, query))
    }
}

/// Where a composed body will land once it leaves the editor.
pub enum Target {
    /// A span of diff rows. `replacing` names the draft being reopened, so
    /// editing revises it instead of stacking a second comment.
    Line {
        anchor: Anchor,
        rows: RangeInclusive<usize>,
        replacing: Option<usize>,
    },
    /// A reply under an existing thread, addressed to its first comment.
    Reply { in_reply_to: u64 },
    /// The whole file. `replacing` names the draft being revised, since a file
    /// takes one remark rather than a stack of them.
    File { replacing: Option<usize> },
}

/// An in-progress comment: the editor buffer plus where it will land.
pub struct Composer {
    pub editor: CommentEditor,
    pub target: Target,
    pub path: Arc<str>,
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
    focused_thread: Option<Arc<str>>,
    diff_scroll: usize,
}

/// The patches drive the whole review surface, so what the file pane shows
/// when it is empty depends on why it is empty.
enum FilesState {
    Loading,
    Loaded,
    Failed,
}

pub struct App {
    pub pr: Option<PullRequest>,
    /// Shared so the highlighting thread reads the same patches the diff is
    /// drawn from rather than a copy of them.
    pub files: Arc<[ChangedFile]>,
    pub threads_by_path: HashMap<Arc<str>, Vec<ReviewThread>>,
    pub drafts: Vec<Draft>,

    pub mode: Mode,
    pub selection: Option<Selection>,
    pub composer: Option<Composer>,
    pub submission: Option<Submission>,
    /// The review handed to the network, held so a rejection can give the
    /// summary back instead of making the user retype it.
    sending: Option<Submission>,
    pub file_filter: Option<CommentEditor>,
    pub search: Option<CommentEditor>,
    pub selected_file: usize,
    pub cursor: usize,
    pub focused_thread: Option<Arc<str>>,
    pub expanded_thread: Option<Arc<str>>,
    pub thread_scroll: usize,
    /// First virtual row of the diff pane on screen. Rows are not source lines:
    /// a line's threads occupy rows of their own, so the offset addresses the
    /// row list the layout builds rather than the patch.
    pub diff_scroll: usize,
    pub pane: Pane,
    pub is_files_visible: bool,

    pub status: String,
    pub loading_frame: usize,
    pub should_quit: bool,
    /// Requests handed to the event loop but not yet answered.
    pub in_flight: usize,

    outbox: Vec<Request>,
    theme: Theme,
    /// Keyed by path, which is what a file is. A position would only mean
    /// anything for as long as the list it indexes stays put.
    highlights: HashMap<Arc<str>, Highlight>,
    filter_snapshot: Option<FileFilterSnapshot>,
    search_origin: Option<SearchOrigin>,
    files_state: FilesState,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self::with_theme(Theme::dark())
    }

    pub fn with_theme(theme: Theme) -> Self {
        Self {
            pr: None,
            files: Arc::from([]),
            threads_by_path: HashMap::new(),
            drafts: Vec::new(),
            mode: Mode::Normal,
            selection: None,
            composer: None,
            submission: None,
            sending: None,
            file_filter: None,
            search: None,
            selected_file: 0,
            cursor: 0,
            focused_thread: None,
            expanded_thread: None,
            thread_scroll: 0,
            diff_scroll: 0,
            pane: Pane::Files,
            is_files_visible: true,
            status: String::new(),
            loading_frame: 0,
            should_quit: false,
            in_flight: 0,
            outbox: Vec::new(),
            theme,
            highlights: HashMap::new(),
            filter_snapshot: None,
            search_origin: None,
            files_state: FilesState::Loading,
        }
    }

    pub const fn theme(&self) -> Theme {
        self.theme
    }

    pub const fn is_loading(&self) -> bool {
        matches!(self.files_state, FilesState::Loading)
    }

    pub const fn advance_loading(&mut self) {
        self.loading_frame = self.loading_frame.wrapping_add(1);
    }

    /// File patches are the only data required to make the main review surface
    /// useful. PR metadata and review threads may arrive independently later.
    pub fn set_files(&mut self, files: Vec<ChangedFile>) {
        // A path that comes back with a new patch cannot keep its old colors.
        self.highlights.clear();
        self.files = files.into();
        self.files_state = FilesState::Loaded;
    }

    /// Threads arrive over GraphQL, which stays up independently of the REST
    /// endpoint the patches come from, so a failed diff still leaves a review
    /// surface worth showing — as long as it does not claim the PR is empty.
    pub fn fail_files(&mut self) {
        self.files = Arc::from([]);
        self.files_state = FilesState::Failed;
    }

    /// The bar paints trouble red. A failure carries the `error:` label; a
    /// GitHub incident names itself and needs none. Derived from the text
    /// rather than tracked alongside it, so the two cannot drift apart.
    pub fn is_status_alarming(&self) -> bool {
        self.status.starts_with("error:") || self.status.starts_with("github ")
    }

    pub const fn files_placeholder(&self) -> &'static str {
        match self.files_state {
            FilesState::Failed => "diff unavailable",
            _ => "no changed files",
        }
    }

    /// Swap the complete renderer palette and discard syntax colors produced
    /// under the previous terminal appearance.
    pub fn set_theme_mode(&mut self, mode: ThemeMode) -> bool {
        if self.theme.mode == mode {
            return false;
        }

        self.theme = Theme::for_mode(mode);
        self.highlights.clear();
        true
    }

    /// Files the threads by path, which is the only way anything looks one up.
    /// They move rather than copy: a review's whole comment history is the
    /// heaviest thing the app holds and one owner is enough.
    pub fn set_meta(&mut self, meta: Meta) {
        let mut by_path: HashMap<Arc<str>, Vec<ReviewThread>> = HashMap::new();
        for thread in meta.threads {
            by_path.entry(thread.path.clone()).or_default().push(thread);
        }

        self.threads_by_path = by_path;
        self.pr = Some(meta.pr);
    }

    pub fn current_file(&self) -> Option<&ChangedFile> {
        self.files.get(self.selected_file)
    }

    pub fn current_path(&self) -> Option<&str> {
        self.current_file().map(|file| &*file.path)
    }

    pub fn focus(&self) -> Focus<'_> {
        Focus {
            cursor: self.cursor,
            selection: self.selection,
            pane: self.pane,
            thread: self.focused_thread.as_deref(),
            expanded: self.expanded_thread.as_deref(),
            query: self.search_query(),
        }
    }

    pub fn open(&self) -> Option<OpenFile<'_>> {
        let patch = self.current_file()?;

        Some(OpenFile {
            patch,
            threads: self
                .threads_by_path
                .get(&patch.path)
                .map_or(&[], Vec::as_slice),
            drafts: self
                .drafts
                .iter()
                .filter(|draft| draft.path == patch.path)
                .collect(),
            highlight: self.highlights.get(&patch.path),
        })
    }

    pub fn diff_len(&self) -> usize {
        self.current_file().map_or(0, |f| f.lines.len())
    }

    pub fn set_highlight(&mut self, path: Arc<str>, styled: Highlight) {
        self.highlights.insert(path, styled);
    }

    pub fn apply(&mut self, action: &Action, layout: &Layout) {
        match *action {
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
            Action::AcceptSearch => self.accept_search(layout),
            Action::CancelSearch => self.cancel_search(),
            Action::NextMatch => self.jump_match(1, layout),
            Action::PrevMatch => self.jump_match(-1, layout),
            Action::NextFile => self.step_file(1),
            Action::PrevFile => self.step_file(-1),
            Action::NextComment => self.jump_comment(1, layout),
            Action::PrevComment => self.jump_comment(-1, layout),
            Action::Move(motion) => self.travel(motion, layout),

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

            Action::StartComment => self.start_comment(layout),
            Action::StartFileComment => self.start_file_comment(layout),
            Action::CommitComment => self.commit_comment(),
            Action::CancelComment => {
                self.composer = None;
                self.mode = Mode::Normal;
                self.selection = None;
            }
            Action::EditDraft => self.edit_draft(),
            Action::DeleteDraft => self.delete_draft(),
            Action::ToggleResolved => self.toggle_resolved(),

            Action::StartSubmit => self.start_submit(layout),
            Action::CommitSubmit => self.commit_submit(),
            Action::CancelSubmit => {
                self.submission = None;
                self.mode = Mode::Normal;
            }
            Action::CycleEvent(direction) => {
                if let Some(submission) = self.submission.as_mut() {
                    submission.event = submission.event.step(direction);
                }
            }
        }
    }

    /// A focused thread takes a reply; anything else starts a fresh draft over
    /// the cursor line or the visual selection.
    fn start_comment(&mut self, layout: &Layout) {
        if self.pane != Pane::Diff {
            return;
        }

        if let Some(id) = self.focused_thread.clone() {
            self.start_reply(&id, layout);
            return;
        }

        let rows = match self.selection {
            Some(selection) => selection.range(),
            None => self.cursor..=self.cursor,
        };

        let Some(file) = self.current_file() else {
            return;
        };
        let Some(anchor) = draft::anchor_for(file, rows.clone()) else {
            self.status = "cannot comment on that line".into();
            return;
        };

        self.composer = Some(Composer {
            editor: CommentEditor::default(),
            target: Target::Line {
                anchor,
                rows,
                replacing: None,
            },
            path: file.path.clone(),
        });
        self.mode = Mode::Insert;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    /// A file takes a single remark, so `C` revises the existing one rather than
    /// stacking another. Available from the tree too: no line is involved, so
    /// there is nothing the diff pane is needed for.
    fn start_file_comment(&mut self, layout: &Layout) {
        let Some(path) = self.current_file().map(|file| file.path.clone())
        else {
            self.status = "no file selected".into();
            return;
        };

        let replacing = self
            .drafts
            .iter()
            .position(|draft| draft.path == path && draft.is_file_level());
        let mut editor = CommentEditor::default();
        if let Some(index) = replacing {
            editor.set_text(&self.drafts[index].body);
        }

        self.composer = Some(Composer {
            editor,
            target: Target::File { replacing },
            path,
        });
        self.mode = Mode::Insert;
        self.selection = None;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    /// Reopens the draft under the cursor with its body and span intact, so
    /// committing revises it instead of stacking a second comment.
    fn edit_draft(&mut self) {
        if self.pane != Pane::Diff {
            return;
        }

        let Some(index) = self.draft_at_cursor() else {
            self.status = "no draft on this line".into();
            return;
        };

        let draft = &self.drafts[index];
        let Attachment::Lines { rows, anchor } = draft.attachment.clone()
        else {
            return;
        };
        let mut editor = CommentEditor::default();
        editor.set_text(&draft.body);

        self.composer = Some(Composer {
            editor,
            target: Target::Line {
                anchor,
                rows,
                replacing: Some(index),
            },
            path: draft.path.clone(),
        });
        self.mode = Mode::Insert;
        self.selection = None;
    }

    fn start_reply(&mut self, id: &str, layout: &Layout) {
        let Some(thread) = self.thread(id) else {
            return;
        };
        let Some(in_reply_to) = thread.reply_target() else {
            self.status = "this thread cannot be replied to".into();
            return;
        };

        self.composer = Some(Composer {
            editor: CommentEditor::default(),
            target: Target::Reply { in_reply_to },
            path: thread.path.clone(),
        });
        self.mode = Mode::Insert;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    fn thread(&self, id: &str) -> Option<&ReviewThread> {
        self.threads_by_path
            .values()
            .flatten()
            .find(|thread| *thread.id == *id)
    }

    /// The open file's threads, which is what the row list indexes into.
    pub fn file_threads(&self) -> &[ReviewThread] {
        self.current_file()
            .and_then(|file| self.threads_by_path.get(&file.path))
            .map_or(&[], Vec::as_slice)
    }

    fn draft_at_cursor(&self) -> Option<usize> {
        let path = &self.current_file()?.path;

        self.drafts
            .iter()
            .position(|draft| draft.covers(path, self.cursor))
    }

    fn delete_draft(&mut self) {
        if self.pane != Pane::Diff {
            return;
        }

        let Some(index) = self.draft_at_cursor() else {
            self.status = "no draft on this line".into();
            return;
        };

        self.drafts.remove(index);
        self.status = "draft discarded".into();
    }

    fn toggle_resolved(&mut self) {
        let Some(id) = self.focused_thread.clone() else {
            self.status = "no thread selected".into();
            return;
        };
        let Some(thread) = self.thread(&id) else {
            return;
        };

        if !thread.can_resolve {
            self.status = "you cannot resolve this thread".into();
            return;
        }

        let is_resolved = !thread.is_resolved;
        self.send(Request::Resolve {
            thread_id: id.to_string(),
            is_resolved,
        });
        self.status = if is_resolved {
            "resolving…".into()
        } else {
            "unresolving…".into()
        };
    }

    fn start_submit(&mut self, layout: &Layout) {
        self.composer = None;
        self.selection = None;
        // A review GitHub rejected comes back with the summary that was typed
        // for it, so a second attempt revises rather than retypes.
        let rejected = self.sending.take_if(|held| held.error.is_some());
        self.submission = Some(rejected.unwrap_or_default());
        self.mode = Mode::Submit;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    /// One review goes out at a time. A second would post twice, since the
    /// drafts only retire once the first is answered.
    fn is_review_sending(&self) -> bool {
        self.sending
            .as_ref()
            .is_some_and(|held| held.error.is_none())
    }

    /// A refused submission leaves the overlay open, so a missing summary is
    /// typed rather than retyped.
    fn commit_submit(&mut self) {
        let Some(mut submission) = self.submission.take() else {
            return;
        };

        let event = submission.event;
        let body = submission.editor.text().trim().to_string();

        // An approval is a verdict in itself, so a bare one with no summary and
        // no inline comments is the whole point rather than an empty review.
        let refusal = if self.is_review_sending() {
            Some("a review is already going out".to_string())
        } else if body.is_empty() && event.requires_body() {
            Some(format!("{} needs a summary", event.label()))
        } else {
            None
        };

        if let Some(refusal) = refusal {
            self.status = refusal;
            self.submission = Some(submission);
            return;
        }

        submission.error = None;
        self.sending = Some(submission);
        self.mode = Mode::Normal;

        let comments: Vec<serde_json::Value> =
            self.drafts.iter().map(Draft::to_api).collect();

        self.send(Request::Review {
            event,
            body,
            comments,
        });
        self.status = format!("submitting {}…", event.label());
    }

    fn send(&mut self, request: Request) {
        self.outbox.push(request);
        self.in_flight += 1;
    }

    /// Drained by the event loop, which owns the network.
    pub fn take_requests(&mut self) -> Vec<Request> {
        std::mem::take(&mut self.outbox)
    }

    /// Reports one request's outcome. Drafts survive a failed submission so the
    /// review can be sent again rather than retyped.
    pub fn finish(&mut self, outcome: Result<Sent, Failure>) {
        self.in_flight = self.in_flight.saturating_sub(1);

        self.status = match outcome {
            Ok(Sent::Review(count)) => {
                self.drafts.drain(..count.min(self.drafts.len()));
                self.sending = None;
                "review submitted".into()
            }
            Ok(Sent::Reply) => "reply posted".into(),
            Ok(Sent::Resolution(true)) => "thread resolved".into(),
            Ok(Sent::Resolution(false)) => "thread unresolved".into(),
            Err(failure) => {
                let status = format!("error: {}", failure.message());
                if let Failure::Review(error) = failure {
                    self.reject_review(error);
                }

                status
            }
        };
    }

    /// A rejected review keeps everything it was made of. The summary goes back
    /// into the overlay with GitHub's reason above it, since the reason names a
    /// field and a rule and the status bar shows one line of it.
    fn reject_review(&mut self, error: String) {
        let Some(submission) = self.sending.as_mut() else {
            return;
        };

        submission.error = Some(error);

        // Reopening mid-edit would steal the keyboard from whatever the user
        // moved on to; the overlay waits for the next `s` instead.
        if self.mode == Mode::Normal && self.composer.is_none() {
            self.submission = self.sending.take();
            self.mode = Mode::Submit;
        }
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
            Some(query) => {
                self.file_filter.get_or_insert_default().set_text(query);
            }
            None => self.file_filter = Some(CommentEditor::default()),
        }
        self.mode = Mode::Filter;
    }

    fn accept_file_filter(&mut self) {
        if self.mode != Mode::Filter || self.filtered_file_indices().is_empty()
        {
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
            query: self.search_query().map(str::to_string),
            cursor: self.cursor,
            focused_thread: self.focused_thread.clone(),
            diff_scroll: self.diff_scroll,
        });
        self.search = Some(CommentEditor::default());
        self.pane = Pane::Diff;
        self.mode = Mode::Search;
        self.selection = None;
    }

    fn accept_search(&mut self, layout: &Layout) {
        if self.mode != Mode::Search {
            return;
        }

        self.search_origin = None;
        self.mode = Mode::Normal;

        let Some(query) = self.search_query().filter(|query| !query.is_empty())
        else {
            self.search = None;
            return;
        };

        if self.search_matches(layout).is_empty() {
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
    pub fn sync_search(&mut self, layout: &Layout) {
        let Some(origin) = self.search_origin.as_ref() else {
            return;
        };

        self.cursor = origin.cursor;
        self.diff_scroll = origin.diff_scroll;

        let matches = self.search_matches(layout);
        let Some(hit) = matches
            .iter()
            .find(|hit| hit.row() >= self.cursor)
            .or_else(|| matches.first())
            .cloned()
        else {
            return;
        };

        self.land_on(hit.row(), hit.thread_id(), layout);
    }

    /// The search box holds a single line, so the query is a slice of it.
    pub fn search_query(&self) -> Option<&str> {
        self.search
            .as_ref()
            .and_then(|editor| editor.lines().first())
            .map(String::as_str)
    }

    /// Every hit in the open file, ordered the way the diff renders them: a code
    /// line, then the threads that hang beneath it.
    pub fn search_matches(&self, layout: &Layout) -> Vec<search::Match> {
        let Some(query) = self.search_query().filter(|query| !query.is_empty())
        else {
            return Vec::new();
        };
        let Some(file) = self.current_file() else {
            return Vec::new();
        };

        let threads = self.file_threads();
        let hits: Vec<(usize, Arc<str>)> = layout
            .rows
            .stops()
            .iter()
            .map(|stop| (stop.source, &threads[stop.thread]))
            .filter(|(_, thread)| {
                thread.comments.iter().any(|comment| {
                    search::is_match(&comment.body, query)
                        || search::is_match(&comment.author, query)
                })
            })
            .map(|(row, thread)| (row, thread.id.clone()))
            .collect();

        let mut matches = Vec::new();
        for (row, line) in file.lines.iter().enumerate() {
            if search::is_match(&line.text, query) {
                matches.push(search::Match::Line(row));
            }

            matches.extend(hits.iter().filter(|(hit, _)| *hit == row).map(
                |(_, id)| search::Match::Thread {
                    row,
                    id: id.clone(),
                },
            ));
        }

        matches
    }

    /// One-based cursor position within the match list, plus the total. A zero
    /// position means the cursor is currently between matches.
    pub fn search_summary(&self, layout: &Layout) -> (usize, usize) {
        let matches = self.search_matches(layout);
        let current =
            self.match_position(&matches).map_or(0, |index| index + 1);

        (current, matches.len())
    }

    /// Byte ranges to paint on one diff row, for the renderer.
    fn match_position(&self, matches: &[search::Match]) -> Option<usize> {
        matches.iter().position(|hit| {
            hit.row() == self.cursor && hit.thread_id() == self.focused_thread
        })
    }

    fn jump_match(&mut self, direction: isize, layout: &Layout) {
        let Some(query) = self.search_query().filter(|query| !query.is_empty())
        else {
            self.status = "no search pattern".into();
            return;
        };

        let matches = self.search_matches(layout);
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
        self.land_on(hit.row(), hit.thread_id(), layout);
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
            .filter_map(|(index, file)| {
                file.path.to_lowercase().contains(&query).then_some(index)
            })
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

        match composer.target {
            Target::Reply { in_reply_to } => {
                if body.is_empty() {
                    self.status = "empty reply discarded".into();
                    return;
                }

                self.send(Request::Reply { in_reply_to, body });
                self.status = "sending reply…".into();
            }
            Target::Line {
                anchor,
                rows,
                replacing,
            } => self.save_draft(
                composer.path,
                Attachment::Lines { rows, anchor },
                body,
                replacing,
            ),
            Target::File { replacing } => self.save_draft(
                composer.path,
                Attachment::File,
                body,
                replacing,
            ),
        }
    }

    /// Files a composed body as a draft, revising `replacing` when the composer
    /// was reopened on one. Emptying a reopened draft is how it gets thrown away.
    fn save_draft(
        &mut self,
        path: Arc<str>,
        attachment: Attachment,
        body: String,
        replacing: Option<usize>,
    ) {
        let Some(index) = replacing else {
            if body.is_empty() {
                self.status = "empty comment discarded".into();
                return;
            }

            self.drafts.push(Draft {
                path,
                attachment,
                body,
            });
            self.status = "draft saved".into();
            return;
        };

        if body.is_empty() {
            self.drafts.remove(index);
            self.status = "draft discarded".into();
            return;
        }

        self.drafts[index].body = body;
        self.status = "draft updated".into();
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
        self.expanded_thread =
            (self.expanded_thread.as_deref() != Some(&id)).then_some(id);
        self.thread_scroll = 0;
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

    fn travel(&mut self, motion: Motion, layout: &Layout) {
        if self.pane == Pane::Files {
            self.travel_files(motion, layout.files_viewport());
            return;
        }

        let viewport = layout.diff_viewport();

        // An open conversation captures the cursor: motions scroll through it
        // rather than walking away from it. Visual mode overrides that, since a
        // selection is being extended over code.
        if self.mode != Mode::Visual && self.expanded_thread.is_some() {
            let limit = layout.rows.body_limit();
            self.thread_scroll =
                step(motion, self.thread_scroll, limit + 1, viewport);
            return;
        }

        if self.mode == Mode::Visual {
            self.cursor = step(motion, self.cursor, self.diff_len(), viewport);
        } else {
            match motion {
                Motion::Down(n) => self.move_diff_stops(1, n, layout),
                Motion::Up(n) => self.move_diff_stops(-1, n, layout),
                Motion::HalfPageDown => {
                    self.move_diff_stops(1, viewport / 2, layout);
                }
                Motion::HalfPageUp => {
                    self.move_diff_stops(-1, viewport / 2, layout);
                }
                Motion::Top => {
                    self.cursor = 0;
                    self.set_focused_thread(None);
                }
                Motion::Bottom => {
                    self.cursor = self.diff_len().saturating_sub(1);
                    self.set_focused_thread(None);
                }
            }
        }

        if let Some(selection) = &mut self.selection {
            selection.head = self.cursor;
        }

        self.follow_cursor(layout);
    }

    fn travel_files(&mut self, motion: Motion, viewport: usize) {
        if self.file_filter.is_none() {
            let target =
                step(motion, self.selected_file, self.files.len(), viewport);
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

        let target = step(motion, position, matches.len(), viewport);
        self.set_selected_file(matches[target], false);
    }

    fn move_diff_stops(
        &mut self,
        direction: isize,
        count: usize,
        layout: &Layout,
    ) {
        let max_steps = layout.rows.len();

        for _ in 0..count.min(max_steps) {
            if !self.move_diff_stop(direction, layout) {
                break;
            }
        }
    }

    fn move_diff_stop(&mut self, direction: isize, layout: &Layout) -> bool {
        let ids = self.thread_ids_at(self.cursor, layout);

        if direction > 0 {
            if let Some(focused) = self.focused_thread.as_deref() {
                if let Some(position) =
                    ids.iter().position(|id| **id == *focused)
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
            if let Some(position) = ids.iter().position(|id| **id == *focused) {
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
        let previous = self.thread_ids_at(self.cursor, layout);
        self.set_focused_thread(previous.last().cloned());
        true
    }

    fn jump_comment(&mut self, direction: isize, layout: &Layout) {
        if let Some((row, id)) = self.comment_stop_here(direction) {
            self.land_on(row, Some(id), layout);
            return;
        }

        let Some((index, row, id)) = self.comment_stop_elsewhere(direction)
        else {
            self.status = "no more comments".into();
            return;
        };

        self.set_selected_file(index, false);
        self.land_on(row, Some(id), layout);
    }

    /// Puts the cursor on a diff row, optionally focusing one of its threads,
    /// and scrolls the row into view.
    fn land_on(
        &mut self,
        row: usize,
        thread: Option<Arc<str>>,
        layout: &Layout,
    ) {
        self.pane = Pane::Diff;
        self.selection = None;
        self.cursor = row;
        self.set_focused_thread(thread);
        self.follow_cursor(layout);
        self.status.clear();
    }

    /// The subset of threads that `}` and `{` stop at. Jumps cross files, so
    /// this anchors a file the layout has not laid out.
    fn comment_stops(&self, index: usize) -> Vec<(usize, Arc<str>)> {
        let Some(file) = self.files.get(index) else {
            return Vec::new();
        };
        let threads = self
            .threads_by_path
            .get(&file.path)
            .map_or(&[][..], Vec::as_slice);

        rows::stops_for(file, threads)
            .into_iter()
            .map(|stop| (stop.source, &threads[stop.thread]))
            .filter(|(_, thread)| !thread.is_resolved && !thread.is_outdated)
            .map(|(row, thread)| (row, thread.id.clone()))
            .collect()
    }

    /// A focused thread that is not itself a stop (resolved or outdated) falls
    /// back to the cursor row, so the jump still moves in the right direction.
    fn comment_stop_here(&self, direction: isize) -> Option<(usize, Arc<str>)> {
        let stops = self.comment_stops(self.selected_file);
        let current = self.focused_thread.as_deref().and_then(|focused| {
            stops.iter().position(|(_, id)| **id == *focused)
        });

        let target = match (current, direction > 0) {
            (Some(index), true) => (index + 1 < stops.len()).then(|| index + 1),
            (Some(index), false) => index.checked_sub(1),
            (None, true) => {
                stops.iter().position(|(row, _)| *row >= self.cursor)
            }
            (None, false) => {
                stops.iter().rposition(|(row, _)| *row <= self.cursor)
            }
        }?;

        Some(stops[target].clone())
    }

    fn comment_stop_elsewhere(
        &self,
        direction: isize,
    ) -> Option<(usize, usize, Arc<str>)> {
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

    fn set_focused_thread(&mut self, focused: Option<Arc<str>>) {
        if self.focused_thread != focused {
            self.expanded_thread = None;
            self.thread_scroll = 0;
        }
        self.focused_thread = focused;
    }

    /// The threads hanging under one source line, in the order `j` visits them.
    fn thread_ids_at(&self, source: usize, layout: &Layout) -> Vec<Arc<str>> {
        let threads = self.file_threads();

        layout
            .rows
            .stops_at(source)
            .iter()
            .map(|stop| threads[stop.thread].id.clone())
            .collect()
    }

    /// The row the cursor sits on: a focused thread's summary when one is
    /// focused, otherwise the source line itself.
    fn cursor_row(&self, layout: &Layout) -> usize {
        let threads = self.file_threads();
        let focused = self.focused_thread.as_deref().and_then(|id| {
            let thread = threads.iter().position(|thread| *thread.id == *id)?;
            layout.rows.summary_row(thread)
        });

        focused.unwrap_or_else(|| layout.rows.code_row(self.cursor))
    }

    /// Keeps the cursor inside the viewport with a small scroll-off margin.
    ///
    /// The row list is the one built before this keystroke, which is also the
    /// one the renderer will slice, so a motion and the scroll that follows it
    /// agree even though the list is a frame behind.
    fn follow_cursor(&mut self, layout: &Layout) {
        self.scroll_into_view(layout, layout.diff_viewport());
    }

    fn scroll_into_view(&mut self, layout: &Layout, viewport: usize) {
        if viewport == 0 {
            return;
        }

        let row = self.cursor_row(layout);
        let margin = 3.min(viewport / 4);

        if row < self.diff_scroll + margin {
            self.diff_scroll = row.saturating_sub(margin);
            return;
        }

        let bottom = self.diff_scroll + viewport.saturating_sub(margin + 1);
        if row > bottom {
            self.diff_scroll = (row + margin + 1).saturating_sub(viewport);
        }
    }
}

/// Resolves a motion to an absolute position in a list of `len` items.
fn step(motion: Motion, current: usize, len: usize, viewport: usize) -> usize {
    let last = len.saturating_sub(1);

    match motion {
        Motion::Down(n) => current.saturating_add(n).min(last),
        Motion::Up(n) => current.saturating_sub(n),
        Motion::HalfPageDown => current.saturating_add(viewport / 2).min(last),
        Motion::HalfPageUp => current.saturating_sub(viewport / 2),
        Motion::Top => 0,
        Motion::Bottom => last,
    }
}
