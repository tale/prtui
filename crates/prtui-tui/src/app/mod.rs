pub mod action;
pub mod command;
pub mod draft;
pub mod editor;
pub mod effect;
pub mod ex;
pub mod input;
pub mod keymap;
pub mod keys;
pub mod link;
pub mod mode;
mod navigation;
mod prompt;
pub mod review;
pub mod search;
mod view;

pub use view::View;

use crate::expand::{self, Gap, Place, Reveal};
use crate::layout::Layout;
use crate::renderer::{Segment, Theme, ThemeMode};
use action::Action;
use draft::{Anchor, Attachment, Draft, Sync};
use editor::{CommentEditor, Edit};
use effect::{Effect, FilesState, Loading, Message};
use keymap::{Keymap, Resolution};
use link::{Errand, Link};
use mode::{Mode, Selection};
use prtui_core::{
    ChangedFile, Comment, DiffLine, Meta, PullRequest, ReviewThread,
};
use review::{Request, Sent, Submission};
use search::Query;
use std::collections::{HashMap, HashSet};
use std::ops::{Range, RangeInclusive};
use std::sync::Arc;
use termina::event::KeyEvent;

/// Syntax colors for one file: the segments of each of its lines.
pub type Highlight = Vec<Vec<Segment>>;

/// The file under review, gathered from the four places its parts live.
///
/// The patch, conversation, syntax colors, and local drafts arrive
/// independently. Derived per frame so none has to be kept in step.
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
    Reply { in_reply_to: Arc<str> },
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

    /// The single boundary for work leaving the model. The event loop drains
    /// and executes it without owning any of the ordering policy.
    effects: Vec<Effect>,
    loading: Loading,
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
            effects: Vec::new(),
            loading: Loading::default(),
            theme,
            keymap: Keymap::default(),
            highlights: HashMap::new(),
            blobs: HashMap::new(),
            fetching: HashSet::new(),
            deferred: None,
            filter_snapshot: None,
            search_origin: None,
            command_history: Vec::new(),
            search_history: Vec::new(),
            filter_history: Vec::new(),
            history_cursor: None,
            overlay_scroll: 0,
            overlay_match: None,
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
        self.loading.is_files_pending()
    }

    pub const fn advance_loading(&mut self) {
        self.loading_frame = self.loading_frame.wrapping_add(1);
    }

    /// Starts the two independent initial reads exactly once.
    pub fn start(&mut self) {
        let Some(generation) = self.loading.start() else {
            return;
        };

        self.effects.reserve(2);
        self.effects.push(Effect::FetchFiles);
        self.effects.push(Effect::FetchMeta { generation });
    }

    /// The event loop is the only executor; all policy has already happened by
    /// the time it drains this list.
    pub fn take_effects(&mut self) -> Vec<Effect> {
        std::mem::take(&mut self.effects)
    }

    fn take_selected<T>(
        &mut self,
        mut select: impl FnMut(Effect) -> Result<T, Effect>,
    ) -> Vec<T> {
        let held = std::mem::take(&mut self.effects);
        let mut retained = Vec::with_capacity(held.len());
        let mut selected = Vec::with_capacity(held.len());

        for effect in held {
            match select(effect) {
                Ok(value) => selected.push(value),
                Err(effect) => retained.push(effect),
            }
        }

        self.effects = retained;
        selected
    }

    fn record_initial_failure(&mut self, failure: String) {
        if self.loading.fail(failure) {
            self.effects.push(Effect::ProbeOutage);
        }
    }

    /// Applies one completed effect and queues any follow-up it requires.
    /// Returns whether a visible value changed.
    pub fn receive(&mut self, message: Message) -> bool {
        match message {
            Message::Files(outcome) => {
                let pending_before = self.loading.pending();

                match outcome {
                    Ok(files) => {
                        self.set_files(files);
                        self.effects.push(Effect::HighlightAll);
                    }
                    Err(error) => {
                        self.fail_files();
                        self.record_initial_failure(error);
                        self.status = self.loading.status();
                    }
                }

                if pending_before != 0 && self.loading.pending() == 0 {
                    self.status = self.loading.status();
                }
                true
            }
            Message::Meta {
                generation,
                outcome,
            } => match self.loading.complete_meta(generation) {
                effect::MetaCompletion::Ignore => false,
                effect::MetaCompletion::Retry(generation) => {
                    self.effects.push(Effect::FetchMeta { generation });
                    false
                }
                effect::MetaCompletion::Accept => {
                    let pending_before = self.loading.pending();
                    let is_initial = self.loading.is_meta_pending();
                    self.loading.meta_ready();

                    match outcome {
                        Ok(meta) => self.set_meta(*meta),
                        Err(error) if is_initial => {
                            self.record_initial_failure(error);
                        }
                        Err(error) => {
                            self.status =
                                format!("error: refreshing comments: {error}");
                        }
                    }

                    if pending_before != 0 && self.loading.pending() == 0 {
                        self.status = self.loading.status();
                    }
                    true
                }
            },
            Message::Request(outcome) => {
                let sent = outcome.as_ref().ok();
                let needs_refetch = sent.is_some_and(Sent::needs_refetch);
                let invalidates = sent.is_some_and(Sent::invalidates_fetch);
                self.finish(outcome);

                if invalidates {
                    self.loading.invalidate_meta();
                }
                if needs_refetch
                    && let Some(generation) = self.loading.request_meta()
                {
                    self.effects.push(Effect::FetchMeta { generation });
                }
                true
            }
            Message::Outage(summary) => {
                self.loading.set_outage(summary);
                self.status = self.loading.status();
                true
            }
        }
    }

    pub const fn take_failure(&mut self) -> Option<String> {
        self.loading.take_failure()
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
        self.loading.files = FilesState::Loaded;
        self.reseed_drafts();
    }

    /// A failed diff still leaves the independently loaded metadata visible.
    pub fn fail_files(&mut self) {
        self.files.clear();
        self.loading.files = FilesState::Failed;
    }

    /// Whether the status bar should paint its text as a failure.
    pub fn is_status_alarming(&self) -> bool {
        self.status.starts_with("error:") || self.status.starts_with("outage:")
    }

    pub const fn files_placeholder(&self) -> &'static str {
        match self.loading.files {
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
        self.effects.push(Effect::HighlightAll);
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
        self.take_selected(|effect| match effect {
            Effect::Highlight(path) => Ok(path),
            effect => Err(effect),
        })
    }

    pub fn take_errands(&mut self) -> Vec<Errand> {
        self.take_selected(|effect| match effect {
            Effect::Errand(errand) => Ok(errand),
            effect => Err(effect),
        })
    }

    /// What the cursor is on, addressed on the web.
    ///
    /// A conversation names itself, code names the file at the head commit,
    /// and anything else names the pull request. The commit is what makes it a
    /// permalink: a branch moves out from under one.
    fn permalink(&self) -> Link {
        if let Some(link) = self.comment_link() {
            return link;
        }

        self.code_link().unwrap_or(Link::PullRequest)
    }

    fn comment_link(&self) -> Option<Link> {
        let id = self.focused_card.as_ref()?.thread()?;
        let comment = self.thread(id)?.comments.first()?;

        comment.reply_target.clone().map(Link::Comment)
    }

    fn code_link(&self) -> Option<Link> {
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

        Some(Link::Blob {
            commit: Arc::clone(commit),
            path: Arc::clone(&file.path),
            lines,
        })
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
        self.status = "opening the pull request".into();
        self.effects
            .push(Effect::Errand(Errand::Open(Link::PullRequest)));
    }

    fn yank_link(&mut self) {
        let link = self.permalink();
        self.status = "yanked link".into();
        self.effects.push(Effect::Errand(Errand::Copy(link)));

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
        self.effects.push(Effect::Highlight(path.clone()));
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
            Action::EditLine(edit) => self.edit_line(edit, layout),
        }
    }

    /// Borrows the immutable state consumed by layout and rendering.
    pub fn view(&self) -> View<'_> {
        View::new(self)
    }
}
