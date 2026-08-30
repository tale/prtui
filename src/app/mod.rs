pub mod action;
pub mod command;
pub mod draft;
pub mod editor;
pub mod ex;
pub mod input;
pub mod keymap;
pub mod keys;
pub mod link;
pub mod mode;
pub mod review;
pub mod search;

use crate::expand::{self, Gap, Place, Reveal};
use crate::layout::Layout;
use crate::layout::rows;
use crate::layout::tree::Row as TreeNode;
use crate::model::{
    ChangedFile, Comment, DiffLine, Meta, PullRequest, ReviewThread,
};
use crate::renderer::{Segment, Theme, ThemeMode};
use crate::vim::{step, step_hit};
use action::{Action, Motion};
use draft::{Anchor, Attachment, Draft, Parent, Sync};
use editor::CommentEditor;
use keymap::{Keymap, Resolution};
use link::{Errand, Origin};
use mode::{Mode, Selection};
use review::{Failure, Request, Sent, Submission};
use search::Query;
use std::collections::{HashMap, HashSet};
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;
use termina::event::KeyEvent;

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
}

/// One row of the file tree, with the conversation counts it shows.
pub struct TreeRow<'a> {
    pub file: &'a ChangedFile,
    pub is_selected: bool,
    pub is_viewed: bool,
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
/// A conversation the cursor can rest on.
///
/// A thread is named by the id GitHub gave it and a draft by the local id it
/// was filed under, since an unsent one has no other name. Both take the focus,
/// both expand, and both answer to the same keys.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Card {
    Thread(Arc<str>),
    Draft(u64),
}

impl Card {
    pub fn is_thread(&self, id: &str) -> bool {
        matches!(self, Self::Thread(held) if **held == *id)
    }

    pub const fn is_draft(&self, id: u64) -> bool {
        matches!(self, Self::Draft(held) if *held == id)
    }

    pub const fn draft(&self) -> Option<u64> {
        match self {
            Self::Draft(id) => Some(*id),
            Self::Thread(_) => None,
        }
    }

    pub const fn thread(&self) -> Option<&Arc<str>> {
        match self {
            Self::Thread(id) => Some(id),
            Self::Draft(_) => None,
        }
    }
}

/// This is what a diff row needs to know that is not a property of the row
/// itself, gathered once so the renderers take a value instead of reaching
/// into the app for whatever they like.
#[derive(Clone, Copy)]
pub struct Focus<'a> {
    pub cursor: usize,
    pub selection: Option<Selection>,
    pub pane: Pane,
    pub card: Option<&'a Card>,
    pub expanded: Option<&'a Card>,
    pub query: Option<Query<'a>>,
}

impl Focus<'_> {
    /// The diff cursor gives way while a card holds the focus.
    pub fn is_cursor(&self, row: usize) -> bool {
        self.pane == Pane::Diff && self.card.is_none() && row == self.cursor
    }

    pub fn is_selected(&self, row: usize) -> bool {
        self.selection
            .is_some_and(|selection| selection.contains(row))
    }

    pub fn is_thread_focused(&self, id: &str) -> bool {
        self.card.is_some_and(|card| card.is_thread(id))
    }

    pub fn is_thread_expanded(&self, id: &str) -> bool {
        self.expanded.is_some_and(|card| card.is_thread(id))
    }

    pub fn is_draft_focused(&self, id: u64) -> bool {
        self.card.is_some_and(|card| card.is_draft(id))
    }

    pub fn is_draft_expanded(&self, id: u64) -> bool {
        self.expanded.is_some_and(|card| card.is_draft(id))
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
    /// The body the composer opened on. Escape compares against it rather than
    /// against emptiness, so reopening a draft and changing nothing still
    /// closes on one key.
    original: String,
    /// Set by an escape that had work to lose. The next escape discards; any
    /// other key clears it.
    pub is_discard_armed: bool,
}

impl Composer {
    fn new(editor: CommentEditor, target: Target, path: Arc<str>) -> Self {
        Self {
            original: editor.text(),
            editor,
            target,
            path,
            is_discard_armed: false,
        }
    }

    pub fn is_dirty(&self) -> bool {
        self.editor.text() != self.original
    }
}

struct FileFilterSnapshot {
    selected_file: usize,
}

/// Where the diff sat when a search began, so cancelling undoes the incremental
/// preview instead of stranding the cursor on a match the user rejected.
struct SearchOrigin {
    cursor: usize,
    focused_card: Option<Card>,
    diff_scroll: usize,
    overlay_scroll: usize,
    /// The mode the search was opened from, which is the one accepting or
    /// cancelling returns to. A search started inside a panel has to leave the
    /// reader in that panel.
    mode: Mode,
}

/// What a reveal was asked for, held while the file it needs is on the way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wanted {
    /// One run of hidden lines, by index into the open file's gaps.
    Gap(usize, Reveal),
    /// Every run at once, which puts the whole file on screen.
    File,
}

/// The runs a reveal asks for, paired with how much of each to open.
///
/// Ordered back to front: a splice moves only what is below it, so working
/// upward leaves every gap still waiting at the index it was found at.
fn wanted_gaps(file: &ChangedFile, wanted: Wanted) -> Vec<(Gap, Reveal)> {
    let gaps = expand::gaps(file);

    match wanted {
        Wanted::Gap(index, reveal) => gaps
            .get(index)
            .map(|gap| (*gap, reveal))
            .into_iter()
            .collect(),
        Wanted::File => gaps
            .into_iter()
            .rev()
            .map(|gap| (gap, Reveal::All))
            .collect(),
    }
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
    /// Each file is shared independently with the syntax worker. A reveal can
    /// therefore replace or copy only the patch it changes instead of cloning
    /// every changed file to preserve one worker's snapshot.
    pub files: Vec<Arc<ChangedFile>>,
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
    /// Paths GitHub says the reader has already been through, as of the last
    /// metadata fetch. Owned by the server: a toggle is a write that the
    /// refetch behind it reads back.
    viewed: HashSet<Arc<str>>,
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
    /// The `:` line, while one is open.
    pub command_line: Option<CommentEditor>,
    pub selected_file: usize,
    /// Directories the reader has folded away, keyed by path with its trailing
    /// slash. Held here rather than in the tree so a fold survives a refetch.
    collapsed: HashSet<Arc<str>>,
    /// The heading the tree cursor is resting on, when it is on one rather than
    /// on a file. The same shape as `focused_thread` in the diff: a cursor plus
    /// an optional thing above it that captures the keys.
    tree_directory: Option<Arc<str>>,
    pub cursor: usize,
    /// The conversation the cursor is resting on, if it is on one rather than
    /// on the code. A draft of the reader's own counts: it is a card the same
    /// as any thread.
    pub focused_card: Option<Card>,
    pub expanded_card: Option<Card>,
    pub thread_scroll: usize,
    /// First virtual row of the diff pane on screen. Rows are not source lines:
    /// a line's threads occupy rows of their own, so the offset addresses the
    /// row list the layout builds rather than the patch.
    pub diff_scroll: usize,
    pub pane: Pane,
    pub is_files_visible: bool,

    /// The comments made about the change as a whole, which the overview
    /// reads under the description.
    pub discussion: Vec<Comment>,

    pub status: String,
    pub loading_frame: usize,
    pub should_quit: bool,
    /// Requests handed to the event loop but not yet answered.
    pub in_flight: usize,

    outbox: Vec<Request>,
    /// Links handed to the event loop, which owns the browser and the terminal.
    errands: Vec<Errand>,
    /// Where the pull request lives on the web. Absent only in a test that
    /// never asked for a link.
    origin: Option<Origin>,
    theme: Theme,
    /// The bindings. Configuration the app owns and the view reads, the same
    /// way the theme is.
    keymap: Keymap,
    /// Keyed by path, which is what a file is. A position would only mean
    /// anything for as long as the list it indexes stays put.
    highlights: HashMap<Arc<str>, Highlight>,
    /// The file at head, split into lines, for the paths whose gaps have been
    /// opened. Kept so a second expansion in the same file costs nothing.
    blobs: HashMap<Arc<str>, Arc<[String]>>,
    /// Paths whose contents are on the way, so a gap opened while the first is
    /// in flight waits on the same answer rather than asking twice.
    fetching: HashSet<Arc<str>>,
    /// The expansion waiting on a file's contents. One at a time: a gap is
    /// named by where it sits in the patch, and revealing one moves the rest.
    deferred: Option<(Arc<str>, Wanted)>,
    /// Paths whose patch has grown since it was colored. Drained by the event
    /// loop, which owns the syntax pass.
    recolor: Vec<Arc<str>>,
    filter_snapshot: Option<FileFilterSnapshot>,
    search_origin: Option<SearchOrigin>,
    /// Every `:` line run this session, oldest first.
    /// What each prompt has been given this session, oldest first. `/` and `:`
    /// open clean, so recall is the only way back to an earlier one.
    command_history: Vec<String>,
    search_history: Vec<String>,
    filter_history: Vec<String>,
    /// How far back through the history the open `:` line has been walked.
    history_cursor: Option<usize>,
    /// How far the open panel has been scrolled.
    pub overlay_scroll: usize,
    /// Which of the panel's hits `n` last landed on, as an index into them.
    overlay_match: Option<usize>,
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
            files: Vec::new(),
            threads_by_path: HashMap::new(),
            drafts: Vec::new(),
            pending_review: None,
            pending_threads: Vec::new(),
            retired: HashSet::new(),
            viewed: HashSet::new(),
            next_draft_id: 0,
            mode: Mode::Normal,
            selection: None,
            composer: None,
            submission: None,
            sending: None,
            file_filter: None,
            search: None,
            command_line: None,
            selected_file: 0,
            collapsed: HashSet::new(),
            tree_directory: None,
            cursor: 0,
            focused_card: None,
            expanded_card: None,
            thread_scroll: 0,
            diff_scroll: 0,
            pane: Pane::Files,
            is_files_visible: true,
            discussion: Vec::new(),
            status: String::new(),
            loading_frame: 0,
            should_quit: false,
            in_flight: 0,
            outbox: Vec::new(),
            errands: Vec::new(),
            origin: None,
            theme,
            keymap: Keymap::default(),
            highlights: HashMap::new(),
            blobs: HashMap::new(),
            fetching: HashSet::new(),
            deferred: None,
            recolor: Vec::new(),
            filter_snapshot: None,
            search_origin: None,
            command_history: Vec::new(),
            search_history: Vec::new(),
            filter_history: Vec::new(),
            history_cursor: None,
            overlay_scroll: 0,
            overlay_match: None,
            files_state: FilesState::Loading,
        }
    }

    pub const fn theme(&self) -> Theme {
        self.theme
    }

    pub const fn keymap(&self) -> &Keymap {
        &self.keymap
    }

    /// Feeds one key to the bindings. The mode is the keymap's addressing, so
    /// the app supplies it rather than the caller.
    pub fn resolve_key(&mut self, key: KeyEvent) -> Resolution {
        self.keymap.resolve(self.mode, key)
    }

    /// Drops a half-typed command, which is what a mode change means for one.
    pub fn clear_pending(&mut self) {
        self.keymap.clear();
    }

    pub fn pending_hint(&self) -> String {
        self.keymap.pending_hint()
    }

    pub const fn is_loading(&self) -> bool {
        matches!(self.files_state, FilesState::Loading)
    }

    pub const fn advance_loading(&mut self) {
        self.loading_frame = self.loading_frame.wrapping_add(1);
    }

    /// File patches are the only data required to make the main review surface
    /// useful. PR metadata and review threads may arrive independently later.
    pub fn set_files<I>(&mut self, files: I)
    where
        I: IntoIterator,
        I::Item: Into<Arc<ChangedFile>>,
    {
        // A path that comes back with a new patch cannot keep its old colors.
        self.highlights.clear();
        self.files = files.into_iter().map(Into::into).collect();
        self.files_state = FilesState::Loaded;
        self.reseed_drafts();
    }

    /// Threads arrive over GraphQL, which stays up independently of the REST
    /// endpoint the patches come from, so a failed diff still leaves a review
    /// surface worth showing — as long as it does not claim the PR is empty.
    pub fn fail_files(&mut self) {
        self.files.clear();
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
        self.viewed = meta.viewed;
        self.pending_review = meta.pending_review;
        self.discussion = meta.discussion;
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
    ///
    /// A comment GitHub has already named keeps the local id it was drawn
    /// under. That id is what the focus and a reopened composer address, and a
    /// refetch lands after every write, so minting a new one each time would
    /// pull the cursor off the card under it.
    fn reseed_drafts(&mut self) {
        let known: HashMap<Arc<str>, u64> = self
            .drafts
            .iter()
            .filter_map(|draft| Some((draft.remote.clone()?, draft.id)))
            .collect();

        let files: HashMap<&str, &ChangedFile> = self
            .files
            .iter()
            .map(|file| (&*file.path, &**file))
            .collect();

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
            draft.id = draft
                .remote
                .as_ref()
                .and_then(|comment| known.get(comment).copied())
                .unwrap_or_else(|| self.take_draft_id());
        }

        seeded.append(&mut in_flight);
        self.drafts = seeded;
        self.prune_focus();
    }

    /// Drops a focus naming a draft that is no longer held. A card the cursor
    /// rests on takes the cursor with it, so one that stopped existing would
    /// leave nothing on screen marked at all.
    fn prune_focus(&mut self) {
        let Some(id) = self.focused_card.as_ref().and_then(Card::draft) else {
            return;
        };

        if self.draft_by_id(id).is_none() {
            self.set_focus(None);
        }
    }

    const fn take_draft_id(&mut self) -> u64 {
        self.next_draft_id += 1;
        self.next_draft_id
    }

    pub fn current_file(&self) -> Option<&ChangedFile> {
        self.files.get(self.selected_file).map(AsRef::as_ref)
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

    /// Conversations on a file that are still open, which is what the tree
    /// marks and what a folded directory has to answer for.
    pub fn unresolved_threads(&self, path: &str) -> usize {
        self.threads_by_path.get(path).map_or(0, |threads| {
            threads.iter().filter(|thread| !thread.is_resolved).count()
        })
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
            is_viewed: self.viewed.contains(&file.path),
            threads: threads.len(),
            unresolved: threads.iter().filter(|t| !t.is_resolved).count(),
        })
    }

    pub fn focus(&self) -> Focus<'_> {
        Focus {
            cursor: self.cursor,
            selection: self.selection,
            pane: self.pane,
            card: self.focused_card.as_ref(),
            expanded: self.expanded_card.as_ref(),
            query: self.live_query(),
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
            drafts: self.drafts_for(&patch.path),
            highlight: self.highlights.get(&patch.path),
        })
    }

    pub fn diff_len(&self) -> usize {
        self.current_file().map_or(0, |f| f.lines.len())
    }

    pub fn set_highlight(&mut self, path: Arc<str>, styled: Highlight) {
        self.highlights.insert(path, styled);
    }

    /// The runs of the open file its patch left out, in the order they read.
    ///
    /// The trailing run ends wherever the file ends, which the patch cannot
    /// say. Once the file itself is in hand that run can name a real count, or
    /// go away when the patch already reached the end.
    pub fn gaps(&self) -> Vec<Gap> {
        let Some(file) = self.current_file() else {
            return Vec::new();
        };

        let mut gaps = expand::gaps(file);
        let Some(content) = self.blobs.get(&file.path) else {
            return gaps;
        };

        if let Some(last) = gaps.last_mut()
            && last.place == Place::Trailing
        {
            last.len = Some(last.len_in(content.len()));
        }
        gaps.retain(|gap| gap.len != Some(0));

        gaps
    }

    /// The run of hidden lines the cursor is addressing.
    ///
    /// A gap is drawn on the hunk header below it, so that header is what the
    /// cursor rests on to name one. The trailing gap has no header of its own,
    /// so the last line of the patch stands in for it.
    fn gap_at_cursor(&self) -> Option<usize> {
        let last = self.current_file()?.lines.len().checked_sub(1)?;
        let cursor = self.cursor;

        self.gaps().into_iter().position(|gap| match gap.place {
            Place::Trailing => cursor == last,
            Place::Leading | Place::Between => gap.at == cursor,
        })
    }

    /// Pulls part of the run under the cursor into the diff.
    pub fn expand(&mut self, reveal: Reveal) {
        let Some(gap) = self.gap_at_cursor() else {
            self.status = "no hidden lines here".into();
            return;
        };

        self.open_gaps(Wanted::Gap(gap, reveal));
    }

    /// Pulls in every run the open file's patch left out, which is the whole
    /// file rather than the parts of it that changed.
    pub fn expand_file(&mut self) {
        self.open_gaps(Wanted::File);
    }

    /// The file's contents are fetched the first time a gap in it is opened,
    /// and the reveal is replayed once they land; after that every further gap
    /// in the same file opens with no round trip.
    fn open_gaps(&mut self, wanted: Wanted) {
        let Some(file) = self.current_file() else {
            return;
        };
        let path = file.path.clone();

        // A deleted file is not at head to be read, and the patch already
        // carries every line it had.
        if file.status == "removed" {
            self.status = "no file at head to expand".into();
            return;
        }

        if let Some(content) = self.blobs.get(&path).cloned() {
            self.status = self.reveal(&path, wanted, &content);
            return;
        }

        // The commit to read the file at comes with the metadata, which is a
        // separate fetch and may not have landed yet.
        let Some(commit) = self.pr.as_ref().map(|pr| pr.head_oid.clone())
        else {
            self.status = "still loading the pull request".into();
            return;
        };

        self.deferred = Some((path.clone(), wanted));
        if self.fetching.insert(path.clone()) {
            self.send(Request::Blob { path, commit });
            self.status = "loading the file…".into();
        }
    }

    /// Drained by the event loop, which owns the syntax pass. A patch that has
    /// grown has to be colored again: the colors are held one list per line.
    pub fn take_recolor(&mut self) -> Vec<Arc<str>> {
        std::mem::take(&mut self.recolor)
    }

    /// Where the pull request lives on the web. Session configuration the app
    /// is handed once, the same way the theme is.
    pub fn set_origin(&mut self, origin: Origin) {
        self.origin = Some(origin);
    }

    pub fn take_errands(&mut self) -> Vec<Errand> {
        std::mem::take(&mut self.errands)
    }

    /// What the cursor is on, addressed on the web.
    ///
    /// A conversation names itself, code names the file at the head commit,
    /// and anything else names the pull request. The commit is what makes it a
    /// permalink: a branch moves out from under one.
    fn permalink(&self) -> Option<String> {
        let origin = self.origin.as_ref()?;

        if let Some(url) = self.comment_url(origin) {
            return Some(url);
        }

        Some(self.code_url(origin).unwrap_or_else(|| origin.pull_url()))
    }

    fn comment_url(&self, origin: &Origin) -> Option<String> {
        let id = self.focused_card.as_ref()?.thread()?;
        let comment = self.thread(id)?.comments.first()?;

        comment.rest_id.map(|rest_id| origin.comment_url(rest_id))
    }

    fn code_url(&self, origin: &Origin) -> Option<String> {
        let commit = &self.pr.as_ref()?.head_oid;
        let file = self.current_file()?;

        // A file the change deletes is not at head to be linked to.
        if file.status == "removed" {
            return None;
        }

        let lines = match self.pane {
            Pane::Files => None,
            Pane::Diff => self.cursor_lines(),
        };

        Some(origin.blob_url(commit, &file.path, lines))
    }

    /// The new-side numbers the cursor or the selection covers. A span with
    /// nothing on the new side is a run of deletions, which has no line at
    /// head to name, so the link falls back to the file itself.
    fn cursor_lines(&self) -> Option<(u32, u32)> {
        let rows = self
            .selection
            .map_or(self.cursor..=self.cursor, |selection| selection.range());
        let lines = self.current_file()?.lines.get(rows)?;
        let numbers: Vec<u32> =
            lines.iter().filter_map(|line| line.new_line).collect();

        Some((*numbers.iter().min()?, *numbers.iter().max()?))
    }

    /// The pull request itself, never the line under the cursor. A blob page
    /// opened mid-review drops the reader out of the review; the page they
    /// want is the one they are already reading.
    fn open_link(&mut self) {
        let Some(origin) = self.origin.as_ref() else {
            self.status = "nothing to open yet".into();
            return;
        };

        self.status = "opening the pull request".into();
        self.errands.push(Errand::Open(origin.pull_url()));
    }

    fn yank_link(&mut self) {
        let Some(url) = self.permalink() else {
            self.status = "nothing to link to yet".into();
            return;
        };

        self.status = format!("yanked {url}");
        self.errands.push(Errand::Copy(url));

        if self.mode == Mode::Visual {
            self.mode = Mode::Normal;
            self.selection = None;
        }
    }

    fn blob_loaded(
        &mut self,
        path: &Arc<str>,
        lines: &Arc<[String]>,
    ) -> String {
        self.fetching.remove(path);
        self.blobs.insert(path.clone(), lines.clone());

        match self.deferred.take() {
            Some((waiting, wanted)) if waiting == *path => {
                self.reveal(path, wanted, lines)
            }
            deferred => {
                self.deferred = deferred;
                String::new()
            }
        }
    }

    /// Splices a run of the file into the open patch and puts everything
    /// addressed by line back where it now belongs.
    fn reveal(
        &mut self,
        path: &Arc<str>,
        wanted: Wanted,
        content: &[String],
    ) -> String {
        let Some(index) = self.files.iter().position(|file| file.path == *path)
        else {
            return String::new();
        };

        // The worker owns an Arc to this file while it colors it. Copy-on-write
        // preserves that snapshot when necessary, but never touches any other
        // file in the review.
        let file = Arc::make_mut(&mut self.files[index]);
        let mut count = 0;

        // Each splice moves only what is below it, so the cursor follows one
        // gap at a time rather than the whole expansion at once.
        for (gap, how) in wanted_gaps(file, wanted) {
            let before = file.lines.len();
            let Some(revealed) = expand::reveal(file, &gap, how, content)
            else {
                continue;
            };

            count += revealed.count;
            if revealed.at > self.cursor {
                continue;
            }

            let shift = file.lines.len().cast_signed() - before.cast_signed();
            self.cursor = self.cursor.saturating_add_signed(shift);
            self.diff_scroll = self.diff_scroll.saturating_add_signed(shift);
        }

        if count == 0 {
            return "nothing left to expand".into();
        }

        // Colors are held per line and drafts are anchored by line number, so
        // both are stale from the reveal downward.
        self.highlights.remove(path);
        self.recolor.push(path.clone());
        self.reanchor_drafts();

        format!("expanded {count} lines")
    }

    /// Recomputes the rows each draft covers. A draft names its lines by
    /// number, so a reveal leaves the rows it was drawn on pointing elsewhere.
    fn reanchor_drafts(&mut self) {
        let files = &self.files;

        for draft in &mut self.drafts {
            let Some(anchor) = draft.anchor().copied() else {
                continue;
            };
            let Some(file) = files.iter().find(|file| file.path == draft.path)
            else {
                continue;
            };
            let Some(rows) = draft::rows_for(file, &anchor) else {
                continue;
            };

            draft.attachment = Attachment::Lines { rows, anchor };
        }
    }

    pub fn apply(&mut self, action: &Action, layout: &Layout) {
        // Only a second escape discards, so every other key stands the composer
        // back down.
        if !matches!(action, Action::CancelComment)
            && let Some(composer) = self.composer.as_mut()
        {
            composer.is_discard_armed = false;
        }
        if !matches!(action, Action::CancelSubmit)
            && let Some(submission) = self.submission.as_mut()
        {
            submission.is_discard_armed = false;
        }

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
            Action::LeaveThread => self.set_focus(None),
            Action::FocusFiles => self.focus_files(),
            Action::FocusDiff => self.focus_diff(),
            Action::StartFind => self.start_find(),
            Action::ClearFind => self.clear_find(),
            Action::Escape => self.escape(),
            Action::AcceptFileFilter => self.accept_file_filter(),
            Action::CancelFileFilter => self.cancel_file_filter(),
            Action::AcceptSearch => self.accept_search(layout),
            Action::CancelSearch => self.cancel_search(),
            Action::NextMatch(count) => self.jump_match(1, count, layout),
            Action::PrevMatch(count) => self.jump_match(-1, count, layout),
            Action::NextFile(count) => self.step_file(1, count, layout),
            Action::PrevFile(count) => self.step_file(-1, count, layout),
            Action::NextComment(count) => {
                self.jump_comment(1, count, layout);
            }
            Action::PrevComment(count) => {
                self.jump_comment(-1, count, layout);
            }
            Action::Move(motion) => self.travel(motion, layout),

            Action::EnterVisual => {
                if self.pane == Pane::Diff {
                    self.set_focus(None);
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
            Action::CancelComment => self.cancel_comment(),
            Action::EditDraft => self.edit_draft(layout),
            Action::DeleteDraft => self.delete_draft(),
            Action::ToggleResolved => self.toggle_resolved(),
            Action::ToggleViewed => self.toggle_viewed(layout),
            Action::Expand(reveal) => self.expand(reveal),
            Action::ExpandFile => self.expand_file(),

            Action::StartSubmit => self.start_submit(layout),
            Action::CommitSubmit => self.commit_submit(),
            Action::CancelSubmit => self.cancel_submit(),
            Action::CycleEvent(direction) => {
                if let Some(submission) = self.submission.as_mut() {
                    submission.event = submission.event.step(direction);
                }
            }

            Action::OpenHelp => {
                self.mode = Mode::Help;
                self.overlay_scroll = 0;
            }
            Action::OpenOverview => {
                self.mode = Mode::Overview;
                self.overlay_scroll = 0;
            }
            Action::CloseOverlay => {
                self.mode = Mode::Normal;
                self.overlay_scroll = 0;
                self.overlay_match = None;
            }
            Action::OpenInBrowser => self.open_link(),
            Action::YankLink => self.yank_link(),
            Action::StartCommandLine => self.start_command_line(),
            Action::RunCommandLine => self.run_command_line(layout),
            Action::CancelCommandLine => self.cancel_command_line(),
            Action::WalkHistory(direction) => {
                self.walk_history(direction, layout);
            }
        }
    }

    /// A focused thread takes a reply; anything else starts a fresh draft over
    /// the cursor line or the visual selection. A focused draft of the reader's
    /// own is not one of those: `c` composes and `e` revises, so a drafted line
    /// still takes a second comment.
    fn start_comment(&mut self, layout: &Layout) {
        if self.pane != Pane::Diff {
            return;
        }

        if let Some(id) = self.focused_card.as_ref().and_then(Card::thread) {
            let id = id.clone();
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
        let path = file.path.clone();
        let Some(anchor) = draft::anchor_for(file, rows.clone()) else {
            self.status = "cannot comment on that line".into();
            return;
        };

        // The rows the note will cover stay painted while it is written, so the
        // composer never floats free of what it answers to.
        self.selection = Some(Selection {
            anchor: *rows.start(),
            head: *rows.end(),
        });
        self.composer = Some(Composer::new(
            CommentEditor::default(),
            Target::Line {
                anchor,
                rows,
                replacing: None,
            },
            path,
        ));
        self.mode = Mode::Insert;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    /// A file takes a single remark, so `C` revises the existing one rather than
    /// stacking another. Available from the tree too: no line is involved, so
    /// there is nothing the diff pane is needed for. The focus follows it over
    /// to the diff, since that is where the typing is about to happen.
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

        if let Some(draft) = existing {
            let id = draft.id;
            self.reopen_draft(id, layout);
            return;
        }

        self.pane = Pane::Diff;
        self.composer = Some(Composer::new(
            CommentEditor::default(),
            Target::File { replacing: None },
            path,
        ));
        self.mode = Mode::Insert;
        self.selection = None;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    /// Reopens the focused draft, or the one under the cursor, with its body
    /// and its span intact, so committing revises it instead of stacking a
    /// second comment.
    fn edit_draft(&mut self, layout: &Layout) {
        let Some(index) = self.editable_draft() else {
            self.status = "no draft here".into();
            return;
        };

        let id = self.drafts[index].id;
        self.reopen_draft(id, layout);
    }

    /// Puts an existing draft back in the composer, whatever it is attached to.
    fn reopen_draft(&mut self, id: u64, layout: &Layout) {
        let Some(index) = self.draft_by_id(id) else {
            return;
        };

        let draft = &self.drafts[index];
        let target = match draft.attachment.clone() {
            Attachment::Lines { rows, anchor } => {
                self.selection = Some(Selection {
                    anchor: *rows.start(),
                    head: *rows.end(),
                });
                Target::Line {
                    anchor,
                    rows,
                    replacing: Some(id),
                }
            }
            Attachment::File => {
                self.selection = None;
                Target::File {
                    replacing: Some(id),
                }
            }
        };

        let mut editor = CommentEditor::default();
        editor.set_text(&draft.body);

        self.pane = Pane::Diff;
        self.composer = Some(Composer::new(editor, target, draft.path.clone()));
        self.mode = Mode::Insert;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    fn start_reply(&mut self, id: &str, layout: &Layout) {
        let Some(thread) = self.thread(id) else {
            return;
        };
        let Some(in_reply_to) = thread.reply_target() else {
            self.status = "this thread cannot be replied to".into();
            return;
        };

        self.composer = Some(Composer::new(
            CommentEditor::default(),
            Target::Reply { in_reply_to },
            thread.path.clone(),
        ));
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

    /// The draft `e` and `d` act on: the focused one when a card holds the
    /// focus, which is the only way to reach a file note, and otherwise the one
    /// covering the cursor line.
    fn editable_draft(&self) -> Option<usize> {
        if self.focused_card.is_some() {
            return self.focused_draft();
        }
        if self.pane != Pane::Diff {
            return None;
        }

        self.draft_at_cursor()
    }

    fn delete_draft(&mut self) {
        let Some(index) = self.editable_draft() else {
            self.status = "no draft here".into();
            return;
        };

        self.discard_draft(index);
        self.prune_focus();
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
        let Some(id) =
            self.focused_card.as_ref().and_then(Card::thread).cloned()
        else {
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

    /// Marking a file read moves on to the next one, since being done with a
    /// file and wanting to keep looking at it are not the same intent. Clearing
    /// the mark stays put: it is asked for by a reader coming back to the file.
    fn toggle_viewed(&mut self, layout: &Layout) {
        let Some(path) = self.current_file().map(|file| file.path.clone())
        else {
            self.status = "no file open".into();
            return;
        };
        let Some(pr) = self.pr.as_ref().map(|pr| pr.id.clone()) else {
            return;
        };

        let is_viewed = !self.viewed.contains(&path);
        self.send(Request::SetViewed {
            pr,
            path,
            is_viewed,
        });

        if !is_viewed {
            self.status = "marking unviewed…".into();
            return;
        }

        self.status = match self.next_unread_file(layout) {
            Some(index) => {
                self.select_file(index);
                "marking viewed…".into()
            }
            None => "marking viewed… nothing left unread".into(),
        };
    }

    /// Takes the mark over from GitHub, which has just confirmed it.
    ///
    /// This is the one piece of server state the app keeps rather than reads
    /// back: a mark says nothing about the threads, so refetching the whole
    /// review to learn one boolean would cost a round trip per file read.
    fn mark_viewed(&mut self, path: Arc<str>, is_viewed: bool) -> String {
        if is_viewed {
            self.viewed.insert(path);
            return "file marked viewed".into();
        }

        self.viewed.remove(&path);
        "file marked unviewed".into()
    }

    /// The next file the reader has not been through, in the order the tree
    /// lists them.
    ///
    /// Files already marked are stepped over rather than landed on. `x` on a
    /// marked file clears its mark, so stopping there would turn a walk down
    /// the review into undoing the last session's work.
    fn next_unread_file(&self, layout: &Layout) -> Option<usize> {
        let files: Vec<usize> = layout.files.files().collect();
        let position = files
            .iter()
            .position(|&index| index == self.selected_file)?;

        files[position + 1..]
            .iter()
            .copied()
            .find(|&index| !self.viewed.contains(&self.files[index].path))
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

    /// A summary is no cheaper to retype than a comment, so escape warns once
    /// here too rather than throwing it away on the first key.
    fn cancel_submit(&mut self) {
        let Some(submission) = self.submission.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };

        let has_summary = !submission.editor.text().trim().is_empty();
        if has_summary && !submission.is_discard_armed {
            submission.is_discard_armed = true;
            self.status = "esc again to discard".into();
            return;
        }

        self.submission = None;
        self.mode = Mode::Normal;
        self.status.clear();
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
            Ok(Sent::Viewed { path, is_viewed }) => {
                self.mark_viewed(path, is_viewed)
            }
            Ok(Sent::Blob { path, lines }) => self.blob_loaded(&path, &lines),
            Err(failure) => {
                let status = format!("error: {}", failure.message());
                match failure {
                    Failure::Review(error) => self.reject_review(error),
                    Failure::Draft(draft, error) => {
                        self.reject_draft(draft, error);
                    }
                    Failure::Blob(path, _) => {
                        self.fetching.remove(&path);
                        self.deferred = None;
                    }
                    Failure::Other(_) => {}
                }

                status
            }
        };

        self.prune_focus();
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
    /// `/` means "find what I am reading". Which surface that is comes from
    /// where the reader is: an open panel, then the tree, then the diff.
    fn start_find(&mut self) {
        if self.mode.is_overlay() || self.pane == Pane::Diff {
            self.start_search();
            return;
        }

        self.start_file_filter();
    }

    /// Backs out of the innermost thing the reader is inside: the conversation
    /// holding the focus, then a live query.
    ///
    /// With nothing left to back out of it says how to leave rather than
    /// leaving. `<Esc>` is what someone presses to escape a state they did not
    /// mean to enter, and answering that by quitting is the one reading of the
    /// key nobody intends.
    fn escape(&mut self) {
        if self.focused_card.is_some() {
            self.set_focus(None);
            return;
        }

        if self.file_filter.is_some() || self.search.is_some() {
            self.clear_find();
            return;
        }

        self.status = "press q to quit".into();
    }

    fn start_command_line(&mut self) {
        self.command_line = Some(CommentEditor::default());
        self.history_cursor = None;
        self.status.clear();
        self.mode = Mode::CommandLine;
    }

    fn cancel_command_line(&mut self) {
        self.command_line = None;
        self.history_cursor = None;
        self.mode = Mode::Normal;
    }

    /// Runs the line and hands the action straight back to `apply`, so a `:`
    /// command and the key bound to the same name take the same path.
    fn run_command_line(&mut self, layout: &Layout) {
        let Some(editor) = self.command_line.take() else {
            self.mode = Mode::Normal;
            return;
        };

        self.history_cursor = None;
        self.mode = Mode::Normal;
        let line = editor.text();
        if self.command_history.last() != Some(&line) && !line.trim().is_empty()
        {
            self.command_history.push(line.clone());
        }

        match ex::parse(&line) {
            Ok(Some(action)) => self.apply(&action, layout),
            Ok(None) => {}
            Err(message) => self.status = message,
        }
    }

    /// Walks the `:` line back through what has been run before. Walking past
    /// the newest entry leaves an empty line, the way Vim does.
    /// What the open prompt recalls from. A mode that is not a prompt has
    /// nothing worth typing twice.
    fn history(&self, mode: Mode) -> &[String] {
        match mode {
            Mode::Filter => &self.filter_history,
            Mode::Search => &self.search_history,
            Mode::CommandLine => &self.command_history,
            _ => &[],
        }
    }

    /// Walks the open prompt's history.
    fn walk_history(&mut self, direction: isize, layout: &Layout) {
        let history = self.history(self.mode);
        if history.is_empty() {
            return;
        }

        let last = history.len() - 1;
        let target = match (self.history_cursor, direction) {
            (None, -1) => Some(last),
            (None, _) => None,
            (Some(0), -1) => Some(0),
            (Some(index), -1) => Some(index - 1),
            (Some(index), _) if index == last => None,
            (Some(index), _) => Some(index + 1),
        };

        let text = target
            .map_or("", |index| history[index].as_str())
            .to_owned();

        // Through the path a keystroke takes, so the tree and the diff preview
        // the recalled text exactly as if it had been typed. That path clears
        // the cursor, so the position is written after it.
        self.edit_prompt(layout, |editor| editor.set_text(&text));
        self.history_cursor = target;
    }

    /// Feeds a key to whichever line the mode is editing. One route for the
    /// five of them, so the router stays a router.
    pub fn type_key(&mut self, key: KeyEvent, layout: &Layout) -> bool {
        self.edit_prompt(layout, |editor| {
            editor.handle_key(key);
        })
    }

    pub fn type_text(&mut self, text: &str, layout: &Layout) -> bool {
        // Every prompt but the composer holds a single line.
        let body = if self.mode == Mode::Insert {
            text.to_owned()
        } else {
            text.replace(['\r', '\n'], "")
        };

        self.edit_prompt(layout, |editor| editor.insert_text(&body))
    }

    fn edit_prompt(
        &mut self,
        layout: &Layout,
        edit: impl FnOnce(&mut CommentEditor),
    ) -> bool {
        // Typing moves off whatever was recalled, so the next recall starts
        // from the end again. Set here rather than per prompt: this is the one
        // path every keystroke into a prompt takes.
        self.history_cursor = None;

        match self.mode {
            Mode::Insert => {
                let Some(composer) = self.composer.as_mut() else {
                    return false;
                };
                composer.is_discard_armed = false;
                edit(&mut composer.editor);
            }
            Mode::Submit => {
                let Some(submission) = self.submission.as_mut() else {
                    return false;
                };
                submission.is_discard_armed = false;
                edit(&mut submission.editor);
            }
            Mode::Filter => {
                let Some(filter) = self.file_filter.as_mut() else {
                    return false;
                };
                edit(filter);
                self.sync_file_filter();
            }
            Mode::Search => {
                let Some(search) = self.search.as_mut() else {
                    return false;
                };
                edit(search);
                self.sync_search(layout);
            }
            Mode::CommandLine => {
                let Some(line) = self.command_line.as_mut() else {
                    return false;
                };
                edit(line);
            }
            Mode::Normal | Mode::Visual | Mode::Help | Mode::Overview => {
                return false;
            }
        }

        true
    }

    /// Drops whichever find is live.
    ///
    /// The pane picks which one goes first when both are; it must not decide
    /// whether anything is cleared at all. A search left highlighted behind the
    /// file tree used to swallow every escape, since the pane said "filter" and
    /// there was no filter to drop.
    fn clear_find(&mut self) {
        let prefers_tree = self.pane == Pane::Files && !self.mode.is_overlay();

        if self.search.is_some()
            && !(prefers_tree && self.file_filter.is_some())
        {
            self.clear_search();
            return;
        }

        self.file_filter = None;
        self.filter_snapshot = None;
    }

    fn clear_search(&mut self) {
        self.search = None;
        self.search_origin = None;
        self.overlay_match = None;
    }

    fn start_file_filter(&mut self) {
        // Whatever was found in a file stops being interesting the moment the
        // reader goes looking for a different file, and its highlights would
        // otherwise sit behind the tree until something else cleared them.
        self.clear_search();

        self.is_files_visible = true;
        self.pane = Pane::Files;

        // `/` opens on the whole tree, the way it opens on an unhighlighted
        // diff. Reopening onto the last query left the list already narrowed
        // by a filter the reader was in the middle of replacing.
        self.filter_snapshot = Some(FileFilterSnapshot {
            selected_file: self.selected_file,
        });
        self.file_filter = Some(CommentEditor::default());
        self.history_cursor = None;
        self.mode = Mode::Filter;
    }

    fn accept_file_filter(&mut self) {
        if self.mode != Mode::Filter || self.filtered_file_indices().is_empty()
        {
            return;
        }

        self.filter_snapshot = None;
        match self.filter_query() {
            Some(query) if query.is_empty() => self.file_filter = None,
            Some(query) if self.filter_history.last() != Some(&query) => {
                self.filter_history.push(query);
            }
            _ => {}
        }
        self.mode = Mode::Normal;
        self.pane = Pane::Files;
    }

    fn cancel_file_filter(&mut self) {
        let snapshot = self.filter_snapshot.take();
        self.file_filter = None;

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
            cursor: self.cursor,
            focused_card: self.focused_card.clone(),
            diff_scroll: self.diff_scroll,
            overlay_scroll: self.overlay_scroll,
            mode: self.mode,
        });
        self.search = Some(CommentEditor::default());
        self.history_cursor = None;

        // A panel floats over the panes, so a search inside one leaves the
        // diff's cursor and selection exactly where they were.
        if !self.mode.is_overlay() {
            self.pane = Pane::Diff;
            self.selection = None;
        }
        self.mode = Mode::Search;
    }

    fn accept_search(&mut self, layout: &Layout) {
        if self.mode != Mode::Search {
            return;
        }

        let origin = self.search_origin.take();
        self.mode = origin.map_or(Mode::Normal, |origin| origin.mode);

        let Some(query) = self.search_query().filter(|query| !query.is_empty())
        else {
            self.search = None;
            return;
        };

        let query = query.to_owned();
        if self.search_history.last() != Some(&query) {
            self.search_history.push(query.clone());
        }

        let is_found = if self.mode.is_overlay() {
            !self.overlay_matches(layout).is_empty()
        } else {
            !self.search_matches(layout).is_empty()
        };

        if !is_found {
            self.status = format!("pattern not found: {query}");
        }
    }

    fn cancel_search(&mut self) {
        let Some(origin) = self.search_origin.take() else {
            self.mode = Mode::Normal;
            return;
        };

        self.mode = origin.mode;
        self.search = None;

        if origin.mode.is_overlay() {
            self.overlay_scroll = origin.overlay_scroll;
            self.overlay_match = None;
            return;
        }

        self.cursor = origin.cursor;
        self.diff_scroll = origin.diff_scroll;
        self.set_focus(origin.focused_card);
    }

    /// Incremental search: every keystroke previews the first match from where
    /// the search began, so the diff tracks the query as it is typed.
    pub fn sync_search(&mut self, layout: &Layout) {
        let Some(origin) = self.search_origin.as_ref() else {
            return;
        };

        if origin.mode.is_overlay() {
            let from = origin.overlay_scroll;
            self.overlay_scroll = from;
            self.overlay_match = None;
            self.land_on_overlay_hit(from, layout);
            return;
        }

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

        self.land_on(hit.row(), hit.card(), layout);
    }

    /// What is being searched for, with its case rule resolved. One pattern
    /// serves every surface, the way Vim's does.
    pub fn live_query(&self) -> Option<Query<'_>> {
        self.search_query().and_then(Query::new)
    }

    /// Which panel is on screen, if any. Not the same as the mode: a search
    /// opened from a panel keeps it up while the prompt is typed into.
    pub fn overlay_mode(&self) -> Option<Mode> {
        if self.mode.is_overlay() {
            return Some(self.mode);
        }

        self.search_origin
            .as_ref()
            .map(|origin| origin.mode)
            .filter(|mode| mode.is_overlay())
    }

    /// Whether the find in progress is aimed at the open panel rather than at
    /// the diff.
    pub fn is_searching_overlay(&self) -> bool {
        self.overlay_mode().is_some()
    }

    /// The panel rows the query hits.
    pub fn overlay_matches(&self, layout: &Layout) -> Vec<usize> {
        let Some(overlay) = layout.overlay.as_ref() else {
            return Vec::new();
        };
        let Some(query) = self.live_query() else {
            return Vec::new();
        };

        overlay.matches(query)
    }

    /// The panel row `n` last landed on, for the view to paint apart from the
    /// rest of the hits.
    pub fn overlay_match_row(&self, layout: &Layout) -> Option<usize> {
        let index = self.overlay_match?;

        self.overlay_matches(layout).get(index).copied()
    }

    fn step_overlay_match(
        &mut self,
        direction: isize,
        count: usize,
        layout: &Layout,
    ) {
        let matches = self.overlay_matches(layout);
        if matches.is_empty() {
            self.status = "pattern not found".into();
            return;
        }

        self.overlay_match =
            step_hit(self.overlay_match, matches.len(), direction, count);

        self.show_overlay_match(&matches, layout);
    }

    /// The first hit at or below where the panel already sits, which is what an
    /// incremental search lands on as it is typed.
    fn land_on_overlay_hit(&mut self, from: usize, layout: &Layout) {
        let matches = self.overlay_matches(layout);
        if matches.is_empty() {
            return;
        }

        let index = matches.iter().position(|row| *row >= from).unwrap_or(0);
        self.overlay_match = Some(index);
        self.show_overlay_match(&matches, layout);
    }

    /// Scrolls the panel the least it can to put the current hit inside it.
    fn show_overlay_match(&mut self, matches: &[usize], layout: &Layout) {
        let Some(&row) = self.overlay_match.and_then(|at| matches.get(at))
        else {
            return;
        };
        let viewport = layout.overlay_viewport().max(1);

        if row < self.overlay_scroll {
            self.overlay_scroll = row;
        } else if row >= self.overlay_scroll + viewport {
            self.overlay_scroll = row + 1 - viewport;
        }

        self.overlay_scroll = self.overlay_scroll.min(layout.overlay_limit());
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
        let Some(query) = self.live_query() else {
            return Vec::new();
        };
        let Some(file) = self.current_file() else {
            return Vec::new();
        };

        let threads = self.file_threads();
        let hits: Vec<(usize, Card)> = layout
            .rows
            .stops()
            .iter()
            .filter(|stop| self.card_matches(&stop.card, threads, query))
            .map(|stop| (stop.source, stop.card.clone()))
            .collect();

        let mut matches = Vec::new();
        for (row, line) in file.lines.iter().enumerate() {
            if query.is_match(&line.text) {
                matches.push(search::Match::Line(row));
            }

            matches.extend(hits.iter().filter(|(hit, _)| *hit == row).map(
                |(_, card)| search::Match::Card {
                    row,
                    card: card.clone(),
                },
            ));
        }

        matches
    }

    /// Whether a card's text answers the query. A draft is the reader's own
    /// words, which is exactly what they are most likely to be looking for.
    fn card_matches(
        &self,
        card: &Card,
        threads: &[ReviewThread],
        query: Query<'_>,
    ) -> bool {
        match card {
            Card::Draft(id) => self
                .draft_by_id(*id)
                .is_some_and(|index| query.is_match(&self.drafts[index].body)),
            Card::Thread(id) => threads
                .iter()
                .find(|thread| thread.id == *id)
                .is_some_and(|thread| {
                    thread.comments.iter().any(|comment| {
                        query.is_match(&comment.body)
                            || query.is_match(&comment.author)
                    })
                }),
        }
    }

    /// One-based cursor position within the match list, plus the total. A zero
    /// position means the cursor is currently between matches.
    pub fn search_summary(&self, layout: &Layout) -> (usize, usize) {
        if self.is_searching_overlay() {
            let matches = self.overlay_matches(layout);
            return (self.overlay_match.map_or(0, |at| at + 1), matches.len());
        }

        let matches = self.search_matches(layout);
        let current =
            self.match_position(&matches).map_or(0, |index| index + 1);

        (current, matches.len())
    }

    /// Byte ranges to paint on one diff row, for the renderer.
    fn match_position(&self, matches: &[search::Match]) -> Option<usize> {
        matches.iter().position(|hit| {
            hit.row() == self.cursor && hit.card() == self.focused_card
        })
    }

    fn jump_match(&mut self, direction: isize, count: usize, layout: &Layout) {
        let Some(query) = self.search_query().filter(|query| !query.is_empty())
        else {
            self.status = "no search pattern".into();
            return;
        };

        // The prompt has its own mode, so where the search was opened from is
        // what says which surface the arrows step through.
        if self.is_searching_overlay() {
            self.step_overlay_match(direction, count, layout);
            return;
        }

        let matches = self.search_matches(layout);
        if matches.is_empty() {
            self.status = format!("pattern not found: {query}");
            return;
        }

        for _ in 0..count {
            // Searching is file-local, so both ends wrap rather than spilling
            // into the next file the way comment jumps do.
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
            self.land_on(hit.row(), hit.card(), layout);
        }
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

    /// Escape leaves the composer, but work is not thrown away on one key: a
    /// changed buffer arms first and says so, and the next escape discards it.
    fn cancel_comment(&mut self) {
        let Some(composer) = self.composer.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };

        if composer.is_dirty() && !composer.is_discard_armed {
            composer.is_discard_armed = true;
            self.status = "esc again to discard".into();
            return;
        }

        self.composer = None;
        self.mode = Mode::Normal;
        self.selection = None;
        self.status.clear();
    }

    fn commit_comment(&mut self) {
        let Some(composer) = self.composer.take() else {
            return;
        };

        let body = composer.editor.text();
        let body = body.trim().to_string();

        self.mode = Mode::Normal;
        self.selection = None;

        let saved = match composer.target {
            Target::Reply { in_reply_to } => {
                if body.is_empty() {
                    self.status = "empty reply discarded".into();
                    return;
                }

                self.send(Request::Reply { in_reply_to, body });
                self.status = "sending reply…".into();
                None
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
        };

        // The note that was just written takes the focus, so it can be read
        // back, reopened, or thrown away without hunting for it. A file note
        // has no line to leave the cursor on, which is what makes this the only
        // way back to one.
        self.set_focus(saved.map(Card::Draft));
    }

    /// Files a composed body as a draft, revising `replacing` when the composer
    /// was reopened on one. Emptying a reopened draft is how it gets thrown away.
    ///
    /// The draft is on screen before GitHub has been told about it, so writing
    /// one is two steps: the local copy, then the request that catches the
    /// server up with it. Answers with the draft that now holds the body, or
    /// nothing when there is no longer one.
    fn save_draft(
        &mut self,
        path: Arc<str>,
        attachment: Attachment,
        body: String,
        replacing: Option<u64>,
    ) -> Option<u64> {
        let Some(index) = replacing.and_then(|id| self.draft_by_id(id)) else {
            if body.is_empty() {
                self.status = "empty comment discarded".into();
                return None;
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
            return Some(id);
        };

        if body.is_empty() {
            self.discard_draft(index);
            return None;
        }

        let draft = &mut self.drafts[index];
        let id = draft.id;
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

        Some(id)
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

        let Some(card) = self.focused_card.clone() else {
            return;
        };
        self.expanded_card =
            (self.expanded_card.as_ref() != Some(&card)).then_some(card);
        self.thread_scroll = 0;
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
    fn step_file(&mut self, direction: isize, count: usize, layout: &Layout) {
        let files: Vec<usize> = layout.files.files().collect();
        let Some(position) =
            files.iter().position(|&index| index == self.selected_file)
        else {
            return;
        };

        let steps =
            direction.saturating_mul(count.min(files.len()).cast_signed());
        let target = position
            .saturating_add_signed(steps)
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
        self.set_focus(None);
        self.diff_scroll = 0;
        self.selection = None;
        if leave_transient_mode {
            self.mode = Mode::Normal;
        }
    }

    fn travel(&mut self, motion: Motion, layout: &Layout) {
        if self.mode.is_overlay() {
            self.overlay_scroll = step(
                motion,
                self.overlay_scroll,
                layout.overlay_limit() + 1,
                layout.overlay_viewport(),
            );
            return;
        }

        if self.pane == Pane::Files {
            self.travel_files(motion, layout);
            return;
        }

        let viewport = layout.diff_viewport();

        // An open conversation captures the cursor while it still has something
        // to scroll. Once it runs out the motion carries on into the diff, so
        // the pane reads as one list and a short conversation never swallows a
        // keypress. Visual mode overrides all of it, since a selection is being
        // extended over code. A line number is not a scroll offset either, so
        // it addresses the file however the cursor is parked.
        if self.mode != Mode::Visual
            && self.expanded_card.is_some()
            && !matches!(motion, Motion::Line(_))
        {
            let limit = layout.rows.body_limit();
            let target = step(motion, self.thread_scroll, limit + 1, viewport);

            // `gg` and `G` mean the ends of the conversation, not the ends of
            // the file, so they stay inside it even when nothing moves.
            if target != self.thread_scroll
                || matches!(motion, Motion::Top | Motion::Bottom)
            {
                self.thread_scroll = target;
                return;
            }
        }

        if self.mode == Mode::Visual {
            self.cursor = match motion {
                Motion::Line(number) => self.row_of_line(number),
                _ => step(motion, self.cursor, self.diff_len(), viewport),
            };
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
                    self.set_focus(None);
                }
                Motion::Bottom => {
                    self.cursor = self.diff_len().saturating_sub(1);
                    self.set_focus(None);
                }
                Motion::Line(number) => {
                    self.cursor = self.row_of_line(number);
                    self.set_focus(None);
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

    /// The diff row a line number names, counted the way the gutter shows it.
    ///
    /// The new side is what a reviewer reads off the screen, so it is tried
    /// first; a line only the old side has still resolves, and a number the
    /// patch never shows lands on the nearest row below it.
    fn row_of_line(&self, number: usize) -> usize {
        let Some(file) = self.current_file() else {
            return 0;
        };
        let wanted = u32::try_from(number).unwrap_or(u32::MAX);
        let last = file.lines.len().saturating_sub(1);

        let found = file
            .lines
            .iter()
            .position(|line| line.new_line == Some(wanted))
            .or_else(|| {
                file.lines
                    .iter()
                    .position(|line| line.old_line == Some(wanted))
            })
            .or_else(|| {
                file.lines.iter().position(|line| {
                    line.new_line.is_some_and(|shown| shown > wanted)
                })
            });

        found.unwrap_or(last)
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
        let cards = Self::cards_at(layout, self.cursor);

        if direction > 0 {
            if let Some(focused) = self.focused_card.as_ref() {
                if let Some(position) =
                    cards.iter().position(|card| card == focused)
                    && let Some(next) = cards.get(position + 1)
                {
                    self.set_focus(Some(next.clone()));
                    return true;
                }
                if self.cursor + 1 < self.diff_len() {
                    self.cursor += 1;
                    self.set_focus(None);
                    return true;
                }
                return false;
            }

            if let Some(first) = cards.first() {
                self.set_focus(Some(first.clone()));
                return true;
            }
            if self.cursor + 1 < self.diff_len() {
                self.cursor += 1;
                return true;
            }
            return false;
        }

        if let Some(focused) = self.focused_card.as_ref() {
            if let Some(position) =
                cards.iter().position(|card| card == focused)
            {
                if position > 0 {
                    self.set_focus(Some(cards[position - 1].clone()));
                } else {
                    self.set_focus(None);
                }
                return true;
            }
            self.set_focus(None);
            return true;
        }

        if self.cursor == 0 {
            return false;
        }
        self.cursor -= 1;
        let previous = Self::cards_at(layout, self.cursor);
        self.set_focus(previous.last().cloned());
        true
    }

    fn jump_comment(
        &mut self,
        direction: isize,
        count: usize,
        layout: &Layout,
    ) {
        for _ in 0..count {
            if !self.jump_comment_once(direction, layout) {
                return;
            }
        }
    }

    /// Returns whether there was a conversation left to land on.
    fn jump_comment_once(&mut self, direction: isize, layout: &Layout) -> bool {
        if let Some((row, card)) = self.comment_stop_here(direction) {
            self.land_on(row, Some(card), layout);
            return true;
        }

        let Some((index, row, card)) = self.comment_stop_elsewhere(direction)
        else {
            self.status = "no more comments".into();
            return false;
        };

        self.set_selected_file(index, false);
        self.land_on(row, Some(card), layout);
        true
    }

    /// Puts the cursor on a diff row, optionally focusing one of its cards,
    /// and scrolls the row into view.
    fn land_on(&mut self, row: usize, card: Option<Card>, layout: &Layout) {
        self.pane = Pane::Diff;
        self.selection = None;
        self.cursor = row;
        self.set_focus(card);
        self.follow_cursor(layout);
        self.status.clear();
    }

    /// The subset of cards that `}` and `{` stop at. Jumps cross files, so
    /// this anchors a file the layout has not laid out.
    ///
    /// A settled conversation is not what a review is being read for, so
    /// resolved and outdated threads are skipped. Every draft is a stop: an
    /// unsent remark is the one thing still waiting on the reader.
    fn comment_stops(&self, index: usize) -> Vec<(usize, Card)> {
        let Some(file) = self.files.get(index) else {
            return Vec::new();
        };
        let threads = self
            .threads_by_path
            .get(&file.path)
            .map_or(&[][..], Vec::as_slice);
        let drafts = self.drafts_for(&file.path);

        rows::stops_for(file, threads, &drafts)
            .into_iter()
            .filter(|stop| {
                stop.card.thread().is_none_or(|id| {
                    threads.iter().any(|thread| {
                        thread.id == *id
                            && !thread.is_resolved
                            && !thread.is_outdated
                    })
                })
            })
            .map(|stop| (stop.source, stop.card))
            .collect()
    }

    /// The drafts filed against one file, in the order the row list indexes
    /// them.
    fn drafts_for(&self, path: &str) -> Vec<&Draft> {
        self.drafts
            .iter()
            .filter(|draft| *draft.path == *path)
            .collect()
    }

    /// A focused card that is not itself a stop (resolved or outdated) falls
    /// back to the cursor row, so the jump still moves in the right direction.
    fn comment_stop_here(&self, direction: isize) -> Option<(usize, Card)> {
        let stops = self.comment_stops(self.selected_file);
        let current = self.focused_card.as_ref().and_then(|focused| {
            stops.iter().position(|(_, card)| card == focused)
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
    ) -> Option<(usize, usize, Card)> {
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

    fn set_focus(&mut self, focused: Option<Card>) {
        if self.focused_card != focused {
            self.expanded_card = None;
            self.thread_scroll = 0;
        }
        self.focused_card = focused;
    }

    /// The focused thread, when the focus is on one rather than on a draft.
    pub fn focused_thread(&self) -> Option<&str> {
        self.focused_card.as_ref()?.thread().map(|id| &**id)
    }

    /// The focused draft, by the index the drafts are held at.
    pub fn focused_draft(&self) -> Option<usize> {
        self.draft_by_id(self.focused_card.as_ref()?.draft()?)
    }

    /// The cards hanging under one source line, in the order `j` visits them.
    fn cards_at(layout: &Layout, source: usize) -> Vec<Card> {
        layout
            .rows
            .stops_at(source)
            .iter()
            .map(|stop| stop.card.clone())
            .collect()
    }

    /// The row the cursor sits on: a focused thread's summary when one is
    /// focused, otherwise the source line itself.
    fn cursor_row(&self, layout: &Layout) -> usize {
        let focused = self
            .focused_card
            .as_ref()
            .and_then(|card| layout.rows.card_row(card));

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
