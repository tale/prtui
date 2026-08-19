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
use crate::layout::tree::Row as TreeNode;
use crate::model::{ChangedFile, DiffLine, Meta, PullRequest, ReviewThread};
use crate::renderer::{Segment, Theme, ThemeMode};
use action::{Action, Motion};
use draft::{Anchor, Attachment, Draft, Parent, Sync};
use editor::CommentEditor;
use mode::{Mode, Selection};
use review::{Failure, Request, Sent, Submission};
use search::Query;
use std::collections::{HashMap, HashSet};
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

/// One row of the file tree, with the conversation counts it shows.
pub struct TreeRow<'a> {
    pub file: &'a ChangedFile,
    pub is_selected: bool,
    pub threads: usize,
    pub unresolved: usize,
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
    pub query: Option<Query<'a>>,
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
        self.query.map_or_else(Vec::new, |query| query.ranges(text))
    }
}

/// Where a composed body will land once it leaves the editor.
pub enum Target {
    /// A span of diff rows. `replacing` names the draft being reopened, so
    /// editing revises it instead of stacking a second comment. It is a draft
    /// id rather than a position: a metadata fetch landing while the composer
    /// is open rebuilds the list, and a position would then name someone else.
    Line {
        anchor: Anchor,
        rows: RangeInclusive<usize>,
        replacing: Option<u64>,
    },
    /// A reply under an existing thread, addressed to its first comment.
    Reply { in_reply_to: u64 },
    /// The whole file. `replacing` names the draft being revised, since a file
    /// takes one remark rather than a stack of them.
    File { replacing: Option<u64> },
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

/// Where a pending thread sits in the diff, in the terms a draft is held in.
///
/// A thread whose line no longer exists in the patch — outdated, or against a
/// file the diff does not carry — has no row to mark, so it is dropped rather
/// than drawn in the wrong place.
fn attachment_for(
    thread: &ReviewThread,
    files: &HashMap<&str, &ChangedFile>,
) -> Option<Attachment> {
    if thread.is_file_level {
        return Some(Attachment::File);
    }

    let end_line = thread.anchor_line()?;
    let anchor = Anchor {
        start_line: thread.start_line.unwrap_or(end_line),
        start_side: thread.start_side.unwrap_or(thread.side),
        end_line,
        side: thread.side,
    };

    let file = files.get(&*thread.path)?;
    let rows = draft::rows_for(file, &anchor)?;

    Some(Attachment::Lines { rows, anchor })
}

pub struct App {
    pub pr: Option<PullRequest>,
    /// Shared so the highlighting thread reads the same patches the diff is
    /// drawn from rather than a copy of them.
    pub files: Arc<[ChangedFile]>,
    pub threads_by_path: HashMap<Arc<str>, Vec<ReviewThread>>,
    /// The pending review's comments, mirrored locally so they can be drawn
    /// before GitHub has answered for them.
    pub drafts: Vec<Draft>,
    /// The review the drafts hang off. Absent until the first draft opens one.
    pending_review: Option<Arc<str>>,
    /// Pending threads exactly as GitHub last reported them. Held apart from
    /// the drafts because turning one into a draft needs the file's patch, and
    /// the two arrive independently.
    pending_threads: Vec<ReviewThread>,
    /// Comments discarded here but possibly still in a metadata fetch that left
    /// before the discard landed. Without this they come back from the dead.
    retired: HashSet<Arc<str>>,
    next_draft_id: u64,

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
    /// Directories the reader has folded away, keyed by path with its trailing
    /// slash. Held here rather than in the tree so a fold survives a refetch.
    collapsed: HashSet<Arc<str>>,
    /// The heading the tree cursor is resting on, when it is on one rather than
    /// on a file. The same shape as `focused_thread` in the diff: a cursor plus
    /// an optional thing above it that captures the keys.
    tree_directory: Option<Arc<str>>,
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
            pending_review: None,
            pending_threads: Vec::new(),
            retired: HashSet::new(),
            next_draft_id: 0,
            mode: Mode::Normal,
            selection: None,
            composer: None,
            submission: None,
            sending: None,
            file_filter: None,
            search: None,
            selected_file: 0,
            collapsed: HashSet::new(),
            tree_directory: None,
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
        self.reseed_drafts();
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
    ///
    /// A pending thread is this session's own unsubmitted work, so it becomes a
    /// draft instead of a conversation on the diff.
    pub fn set_meta(&mut self, meta: Meta) {
        let mut by_path: HashMap<Arc<str>, Vec<ReviewThread>> = HashMap::new();
        let mut pending = Vec::new();

        for thread in meta.threads {
            if !thread.is_pending() {
                by_path.entry(thread.path.clone()).or_default().push(thread);
                continue;
            }

            // A discard that GitHub has already answered can still be missing
            // from a fetch that left before it, so the thread is dropped rather
            // than drawn back onto the diff.
            if !self.is_retired(&thread) {
                pending.push(thread);
            }
        }

        self.retired.retain(|id| {
            pending.iter().any(|thread| thread.comments[0].id == *id)
        });

        self.threads_by_path = by_path;
        self.pending_threads = pending;
        self.pending_review = meta.pending_review;
        self.pr = Some(meta.pr);
        self.reseed_drafts();
        self.create_drafts();
    }

    fn is_retired(&self, thread: &ReviewThread) -> bool {
        thread
            .comments
            .first()
            .is_some_and(|first| self.retired.contains(&first.id))
    }

    /// Rebuilds the drafts from what GitHub last reported.
    ///
    /// A draft the server has not caught up with yet outranks its own stale
    /// copy: the screen is the newer of the two until the write lands, and
    /// dropping the local one would undo an edit in front of the user.
    fn reseed_drafts(&mut self) {
        let files: HashMap<&str, &ChangedFile> =
            self.files.iter().map(|file| (&*file.path, file)).collect();

        let mut seeded: Vec<Draft> = Vec::new();
        for thread in &self.pending_threads {
            let Some(comment) = thread.comments.first() else {
                continue;
            };
            let Some(attachment) = attachment_for(thread, &files) else {
                continue;
            };

            seeded.push(Draft {
                id: 0,
                path: thread.path.clone(),
                attachment,
                body: comment.body.clone(),
                remote: Some(comment.id.clone()),
                sync: Sync::Synced,
            });
        }

        let mut in_flight: Vec<Draft> = std::mem::take(&mut self.drafts)
            .into_iter()
            .filter(|draft| !draft.sync.is_settled())
            .collect();

        seeded.retain(|seed| {
            !in_flight.iter().any(|draft| draft.remote == seed.remote)
        });
        for draft in &mut seeded {
            draft.id = self.take_draft_id();
        }

        seeded.append(&mut in_flight);
        self.drafts = seeded;
    }

    const fn take_draft_id(&mut self) -> u64 {
        self.next_draft_id += 1;
        self.next_draft_id
    }

    pub fn current_file(&self) -> Option<&ChangedFile> {
        self.files.get(self.selected_file)
    }

    pub fn current_path(&self) -> Option<&str> {
        self.current_file().map(|file| &*file.path)
    }

    pub const fn collapsed(&self) -> &HashSet<Arc<str>> {
        &self.collapsed
    }

    pub fn tree_directory(&self) -> Option<&str> {
        self.tree_directory.as_deref()
    }

    pub fn tree_row(&self, index: usize) -> Option<TreeRow<'_>> {
        let file = self.files.get(index)?;
        let threads = self
            .threads_by_path
            .get(&file.path)
            .map_or(&[][..], Vec::as_slice);

        Some(TreeRow {
            file,
            is_selected: index == self.selected_file,
            threads: threads.len(),
            unresolved: threads.iter().filter(|t| !t.is_resolved).count(),
        })
    }

    pub fn focus(&self) -> Focus<'_> {
        Focus {
            cursor: self.cursor,
            selection: self.selection,
            pane: self.pane,
            thread: self.focused_thread.as_deref(),
            expanded: self.expanded_thread.as_deref(),
            query: self.diff_query(),
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
            Action::Activate => self.activate(layout),
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
            Action::NextFile => self.step_file(1, layout),
            Action::PrevFile => self.step_file(-1, layout),
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

        let existing = self
            .drafts
            .iter()
            .find(|draft| draft.path == path && draft.is_file_level());
        let replacing = existing.map(|draft| draft.id);
        let mut editor = CommentEditor::default();
        if let Some(draft) = existing {
            editor.set_text(&draft.body);
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
                replacing: Some(draft.id),
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

        self.discard_draft(index);
    }

    /// Discards a draft, which means asking GitHub to drop the comment holding
    /// it. One whose own creation is still out has no comment id to name yet,
    /// so it is marked and the discard rides on that answer.
    fn discard_draft(&mut self, index: usize) {
        let draft = &mut self.drafts[index];

        if let Sync::Creating { .. } = draft.sync {
            draft.sync = Sync::Deleting;
            self.status = "discarding draft…".into();
            return;
        }

        let Some(comment) = draft.remote.clone() else {
            self.drafts.remove(index);
            self.status = "draft discarded".into();
            return;
        };

        let id = draft.id;
        draft.sync = Sync::Deleting;
        self.retired.insert(comment.clone());
        self.send(Request::DeleteComment { draft: id, comment });
        self.status = "discarding draft…".into();
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
            thread_id: id,
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

        // Drafts are already on GitHub, so the review that publishes them is
        // the one the app opened for them. Without any, the verdict rides alone
        // and files a review of its own.
        let parent = match (&self.pending_review, self.pr.as_ref()) {
            (Some(review), _) => Some(Parent::Review(review.clone())),
            (None, Some(pr)) => Some(Parent::PullRequest(pr.id.clone())),
            (None, None) => None,
        };

        // An approval is a verdict in itself, so a bare one with no summary and
        // no inline comments is the whole point rather than an empty review.
        let refusal = if self.is_review_sending() {
            Some("a review is already going out".to_string())
        } else if body.is_empty() && event.requires_body() {
            Some(format!("{} needs a summary", event.label()))
        } else if self.is_draft_in_flight() {
            Some("a draft is still saving".to_string())
        } else if parent.is_none() {
            Some("the pull request has not loaded yet".to_string())
        } else {
            None
        };

        let (Some(parent), None) = (parent, refusal.as_ref()) else {
            self.status = refusal.unwrap_or_default();
            self.submission = Some(submission);
            return;
        };

        submission.error = None;
        self.sending = Some(submission);
        self.mode = Mode::Normal;

        self.send(Request::Review {
            parent,
            event,
            body,
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
            Ok(Sent::ThreadAdded {
                draft,
                review,
                comment,
            }) => self.draft_created(draft, review, comment),
            Ok(Sent::CommentUpdated(draft)) => self.draft_settled(draft),
            Ok(Sent::CommentDeleted(draft)) => {
                if let Some(index) = self.draft_by_id(draft) {
                    self.drafts.remove(index);
                }

                "draft discarded".into()
            }
            Ok(Sent::Review) => {
                // Everything it carried is GitHub's now, and the refetch that
                // follows brings the whole review back as submitted threads.
                self.drafts.clear();
                self.pending_review = None;
                self.sending = None;
                "review submitted".into()
            }
            Ok(Sent::Reply) => "reply posted".into(),
            Ok(Sent::Resolution(true)) => "thread resolved".into(),
            Ok(Sent::Resolution(false)) => "thread unresolved".into(),
            Err(failure) => {
                let status = format!("error: {}", failure.message());
                match failure {
                    Failure::Review(error) => self.reject_review(error),
                    Failure::Draft(draft, error) => {
                        self.reject_draft(draft, error);
                    }
                    Failure::Other(_) => {}
                }

                status
            }
        };
    }

    /// GitHub has named the draft. Whatever was asked of it while it had no
    /// name — an edit, a discard — goes out now that it has one.
    fn draft_created(
        &mut self,
        draft: u64,
        review: Arc<str>,
        comment: Arc<str>,
    ) -> String {
        self.pending_review = Some(review);

        let Some(index) = self.draft_by_id(draft) else {
            return "draft saved".into();
        };

        self.drafts[index].remote = Some(comment);
        let status = match self.drafts[index].sync.clone() {
            Sync::Deleting => {
                self.drafts[index].sync = Sync::Synced;
                self.discard_draft(index);
                "discarding draft…".into()
            }
            Sync::Creating { is_dirty: true } => {
                self.update_draft(index);
                "saving draft…".into()
            }
            _ => {
                self.drafts[index].sync = Sync::Synced;
                "draft saved".into()
            }
        };

        // The review this opened is what everything still queued was waiting
        // for.
        self.create_drafts();

        status
    }

    fn draft_settled(&mut self, draft: u64) -> String {
        if let Some(index) = self.draft_by_id(draft) {
            self.drafts[index].sync = Sync::Synced;
        }

        "draft saved".into()
    }

    /// A draft the server refused stays on screen carrying the reason. Dropping
    /// it would throw away writing the user cannot get back.
    fn reject_draft(&mut self, draft: u64, error: String) {
        let Some(index) = self.draft_by_id(draft) else {
            return;
        };

        if let Some(comment) = &self.drafts[index].remote {
            self.retired.remove(comment);
        }

        self.drafts[index].sync = Sync::Failed(error);
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

    /// What the diff is being searched for, with its case rule resolved.
    fn diff_query(&self) -> Option<Query<'_>> {
        self.search_query().and_then(Query::new)
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
        let Some(query) = self.diff_query() else {
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
                    query.is_match(&comment.body)
                        || query.is_match(&comment.author)
                })
            })
            .map(|(row, thread)| (row, thread.id.clone()))
            .collect();

        let mut matches = Vec::new();
        for (row, line) in file.lines.iter().enumerate() {
            if query.is_match(&line.text) {
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

    /// The tree searches paths the same way the diff searches code, so a
    /// capital in either box means the same thing.
    pub fn tree_query(&self) -> Option<Query<'_>> {
        self.file_filter
            .as_ref()
            .and_then(|editor| editor.lines().first())
            .and_then(|line| Query::new(line))
    }

    pub fn filtered_file_indices(&self) -> Vec<usize> {
        let Some(query) = self.tree_query() else {
            return (0..self.files.len()).collect();
        };

        self.files
            .iter()
            .enumerate()
            .filter(|(_, file)| query.is_match(&file.path))
            .map(|(index, _)| index)
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
    ///
    /// The draft is on screen before GitHub has been told about it, so writing
    /// one is two steps: the local copy, then the request that catches the
    /// server up with it.
    fn save_draft(
        &mut self,
        path: Arc<str>,
        attachment: Attachment,
        body: String,
        replacing: Option<u64>,
    ) {
        let Some(index) = replacing.and_then(|id| self.draft_by_id(id)) else {
            if body.is_empty() {
                self.status = "empty comment discarded".into();
                return;
            }

            let id = self.take_draft_id();
            self.drafts.push(Draft {
                id,
                path,
                attachment,
                body,
                remote: None,
                sync: Sync::Queued,
            });
            self.status = "saving draft…".into();
            self.create_drafts();
            return;
        };

        if body.is_empty() {
            self.discard_draft(index);
            return;
        }

        let draft = &mut self.drafts[index];
        draft.body = body;

        match draft.sync {
            // Nothing has left yet, so the new body is simply what gets sent.
            Sync::Queued => self.status = "saving draft…".into(),
            // An edit that beats its own creation home has nothing to address
            // itself to, so it rides along on that answer instead.
            Sync::Creating { .. } => {
                draft.sync = Sync::Creating { is_dirty: true };
                self.status = "saving draft…".into();
            }
            _ => self.update_draft(index),
        }
    }

    /// Sends the body of an already-created draft. A draft with no comment id
    /// has nothing to send it to, which only happens after a creation failed.
    fn update_draft(&mut self, index: usize) {
        let draft = &mut self.drafts[index];
        let Some(comment) = draft.remote.clone() else {
            draft.sync = Sync::Queued;
            self.create_drafts();
            return;
        };

        let (id, body) = (draft.id, draft.body.clone());
        draft.sync = Sync::Updating;
        self.send(Request::UpdateComment {
            draft: id,
            comment,
            body,
        });
        self.status = "saving draft…".into();
    }

    /// Sends every draft GitHub has not been told about yet.
    ///
    /// The first one has to open the pending review, and a second sent beside
    /// it would open a second review, so nothing else leaves until that answer
    /// names the review the rest can join.
    fn create_drafts(&mut self) {
        let Some(pull_request) = self.pr.as_ref().map(|pr| pr.id.clone())
        else {
            return;
        };

        let parent = match &self.pending_review {
            Some(review) => Parent::Review(review.clone()),
            None if self.is_draft_in_flight() => return,
            None => Parent::PullRequest(pull_request),
        };

        let queued: Vec<usize> = self
            .drafts
            .iter()
            .enumerate()
            .filter(|(_, draft)| draft.sync == Sync::Queued)
            .map(|(index, _)| index)
            .collect();

        let is_opening = matches!(parent, Parent::PullRequest(_));
        for index in queued {
            let request = Request::AddThread {
                draft: self.drafts[index].id,
                parent: parent.clone(),
                input: self.drafts[index].to_input(&parent),
            };

            self.drafts[index].sync = Sync::Creating { is_dirty: false };
            self.send(request);

            if is_opening {
                return;
            }
        }
    }

    fn is_draft_in_flight(&self) -> bool {
        self.drafts.iter().any(|draft| draft.sync.is_in_flight())
    }

    fn draft_by_id(&self, id: u64) -> Option<usize> {
        self.drafts.iter().position(|draft| draft.id == id)
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

    fn activate(&mut self, layout: &Layout) {
        if self.pane == Pane::Files {
            // A heading has nothing to open, so the same key folds it.
            if self.tree_directory.is_some() {
                self.toggle_directory(layout);
            } else {
                self.focus_diff();
            }
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

    /// `]` and `[` walk files only, skipping the headings `j` stops on, and in
    /// the order the tree lists them rather than the order GitHub sent them.
    fn step_file(&mut self, direction: isize, layout: &Layout) {
        let files: Vec<usize> = layout.files.files().collect();
        let Some(position) =
            files.iter().position(|&index| index == self.selected_file)
        else {
            return;
        };

        let target = position
            .saturating_add_signed(direction)
            .min(files.len().saturating_sub(1));
        self.select_file(files[target]);
    }

    fn set_selected_file(&mut self, index: usize, leave_transient_mode: bool) {
        if self.files.is_empty() {
            return;
        }

        self.selected_file = index.min(self.files.len() - 1);
        self.tree_directory = None;
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
            self.travel_files(motion, layout);
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

    /// The cursor walks headings as well as files, since a heading has to be
    /// reachable to be unfolded.
    fn travel_files(&mut self, motion: Motion, layout: &Layout) {
        let tree = &layout.files;
        if tree.is_empty() {
            return;
        }

        let current = self.tree_cursor(layout);
        let target = step(motion, current, tree.len(), layout.files_viewport());

        match tree.get(target) {
            Some(TreeNode::File { index, .. }) => {
                self.tree_directory = None;
                self.set_selected_file(*index, false);
            }
            Some(TreeNode::Directory { path, .. }) => {
                self.tree_directory = Some(path.clone());
            }
            None => {}
        }
    }

    /// Which row the tree cursor is on: the heading it rests on, or the row of
    /// the open file.
    fn tree_cursor(&self, layout: &Layout) -> usize {
        self.tree_directory
            .as_deref()
            .map_or_else(
                || layout.files.row_of(self.selected_file),
                |path| layout.files.row_of_directory(path),
            )
            .unwrap_or(0)
    }

    /// Folds the heading the cursor is on, or the one the open file sits under.
    /// Unfolding leaves the cursor where it is, so the contents appear below it.
    fn toggle_directory(&mut self, layout: &Layout) {
        let mut path = self.tree_directory.clone();

        if path.is_none() {
            let row = self.tree_cursor(layout);
            // Folding away the file the cursor is on would strand it, so the
            // cursor moves up to the heading swallowing it.
            path = layout.files.enclosing_directory(row).cloned();
            self.tree_directory.clone_from(&path);
        }

        let Some(path) = path else {
            return;
        };

        if !self.collapsed.remove(&path) {
            self.collapsed.insert(path);
        }
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
