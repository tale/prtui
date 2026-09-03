//! The diff pane's virtual row model.
//!
//! The pane is not a list of source lines. A row is a line of the patch, a
//! thread group heading, a hidden-summary marker, a thread summary, or one
//! line inside an expanded conversation. Building that up front is what lets
//! the cursor, the scroll offset, and the renderer all address the same thing,
//! and it is the only place that decides which threads hang under which line.
//!
//! Rows are descriptors, not styled text: the view converts only the visible
//! slice into spans, so rebuilding the list on every keystroke stays cheap even
//! for a very large diff. The one exception is the expanded conversation, whose
//! length depends on how its markdown wraps — that content is rendered here
//! because its row count is a layout fact.

use crate::app::Card;
use crate::app::draft::Draft;
use crate::expand::Gap;
use crate::renderer::Theme;
use crate::renderer::markdown::{self, Block as MarkdownBlock};
use crate::text::measure::text_width;
use crate::text::wrap::{self, Fragment};
use prtui_core::{ChangedFile, DiffLine, LineKind, ReviewThread, Side};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::borrow::Cow;

/// Columns the line-number gutter occupies, which is also how far a thread card
/// is indented so it hangs under the code rather than beside it: two line
/// numbers, the draft or thread mark, and the sigil.
pub const GUTTER: usize = 12;

/// Columns an expanded body gives up to the rail beside it.
///
/// The rail sits two columns in from the card's own marker so it nests under
/// the summary rather than lining up with the markers of the threads stacked
/// around it.
pub const BODY_INDENT: usize = 4;

/// Which pile a thread sits in. Open threads are what review is about, so they
/// sort and render ahead of the settled ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Open,
    Resolved,
    Outdated,
}

impl ThreadState {
    pub const fn of(thread: &ReviewThread) -> Self {
        if thread.is_outdated {
            Self::Outdated
        } else if thread.is_resolved {
            Self::Resolved
        } else {
            Self::Open
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Resolved => "resolved",
            Self::Outdated => "outdated",
        }
    }

    pub const fn marker(self) -> &'static str {
        match self {
            Self::Open => "◆",
            Self::Resolved | Self::Outdated => "◇",
        }
    }

    /// A settled thread is history rather than work, so it is drawn back and
    /// says which kind of settled it is. An open one needs neither.
    pub const fn is_settled(self) -> bool {
        !matches!(self, Self::Open)
    }
}

/// One row of the diff pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// One row's worth of a line of the patch. A line too wide for the pane
    /// folds across several of these, all naming the same `source`.
    Code { source: usize, fragment: Fragment },
    /// A pending note of the reader's own, by index into [`View::drafts`].
    Draft { draft: usize },
    /// A run of the file the patch left out, by index into [`View::gaps`].
    /// Drawn where those lines belong: above the hunk that follows them, or at
    /// the foot of the pane for the run below the last hunk.
    Gap { gap: usize },
    /// The seam between two cards stacked under one line. Without it the gap
    /// between two threads reads as weaker than the gap between two comments
    /// inside one of them.
    Divider,
    /// A thread's one-line summary, by index into the file's thread slice.
    Summary {
        thread: usize,
        state: ThreadState,
        /// Width the author column is padded to across every thread stacked
        /// under this line, so their summaries start together.
        author_width: usize,
    },
    /// One line of the expanded conversation, by index into [`Rows::body`].
    Body { index: usize },
}

/// A rendered line of an expanded conversation. The rail beside it doubles as
/// a scrollbar, so each row knows whether the thumb covers it.
#[derive(Debug, Clone)]
pub struct BodyRow {
    pub spans: Vec<Span<'static>>,
    pub is_thumb: bool,
}

/// A card paired with the source line it hangs under, in the order the cursor
/// visits them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stop {
    pub source: usize,
    pub card: Card,
}

/// Where a card sits relative to the code, which is the order the pane draws
/// them in and therefore the order the cursor walks them in.
///
/// Remarks about the whole file lead the pane, above the first line of the
/// patch. Under a line, the reader's own notes come before the conversations
/// GitHub holds, since an unsent one is what is still being worked on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Class {
    FileDraft,
    FileThread,
    LineDraft,
    LineThread,
}

/// What laying a file out needs from app state.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub focused: Option<&'a Card>,
    pub expanded: Option<&'a Card>,
    /// How far into the expanded conversation the reader has scrolled.
    pub scroll: usize,
    pub width: usize,
    /// Rows the expanded conversation may occupy before it scrolls.
    pub window: usize,
    pub theme: Theme,
    /// Pending notes on this file, in the order the view indexes them.
    pub drafts: &'a [&'a Draft],
    /// Runs of the file the patch left out, in the order they read.
    pub gaps: &'a [Gap],
}

/// A conversation about the file rather than any line in it.
///
/// GitHub leaves both line numbers null for those. An outdated thread also loses
/// `line`, but keeps `originalLine`, which is what tells the two apart.
pub fn is_file_level(thread: &ReviewThread) -> bool {
    !thread.is_outdated && thread.anchor_line().is_none()
}

/// Every card in `file` paired with the line it hangs under, in visit order.
///
/// File-level threads belong to no line and lead the pane, so they pin to the
/// first. Outdated ones have no live anchor and pin after the last, matching
/// where each is drawn. A thread whose anchor is not in the patch at all is
/// dropped: there is no row to reach it from.
pub fn stops_for(
    file: &ChangedFile,
    threads: &[ReviewThread],
    drafts: &[&Draft],
) -> Vec<Stop> {
    stops_from(file, threads, drafts, &resolve_anchors(file, threads))
}

/// `stops_for` once the anchors are already resolved, which is what keeps the
/// row builder from walking the patch a second time for them.
fn stops_from(
    file: &ChangedFile,
    threads: &[ReviewThread],
    drafts: &[&Draft],
    anchored: &[Option<usize>],
) -> Vec<Stop> {
    let last = file.lines.len().saturating_sub(1);

    // Sorted on the same key the pane emits rows in, so the cursor and the
    // drawing agree without either consulting the other.
    let mut stops: Vec<(usize, Class, (bool, usize), Card)> = threads
        .iter()
        .enumerate()
        .filter_map(|(thread, review)| {
            let (source, class) = if is_file_level(review) {
                (0, Class::FileThread)
            } else if review.is_outdated {
                (last, Class::LineThread)
            } else {
                (anchored[thread]?, Class::LineThread)
            };

            // Outdated threads are drawn after the live ones sharing their
            // pinned line; otherwise a thread keeps the place GitHub gave it.
            Some((
                source,
                class,
                (review.is_outdated, thread),
                Card::Thread(review.id.clone()),
            ))
        })
        .collect();

    stops.extend(drafts.iter().enumerate().filter_map(|(index, draft)| {
        let Some(rows) = draft.rows() else {
            return Some((
                0,
                Class::FileDraft,
                (false, index),
                Card::Draft(draft.id),
            ));
        };

        // A note whose span no longer exists in the patch is drawn nowhere, so
        // there is no row for the cursor to reach it from.
        let source = *rows.end();
        (source < file.lines.len()).then_some((
            source,
            Class::LineDraft,
            (false, index),
            Card::Draft(draft.id),
        ))
    }));

    stops.sort_by(|left, right| {
        (left.0, left.1, left.2).cmp(&(right.0, right.1, right.2))
    });

    stops
        .into_iter()
        .map(|(source, _, _, card)| Stop { source, card })
        .collect()
}

/// Threads anchored to one side, sorted by the line number each waits for.
fn waiting_on(threads: &[ReviewThread], side: Side) -> Vec<(u32, usize)> {
    let mut waiting: Vec<(u32, usize)> = threads
        .iter()
        .enumerate()
        .filter(|(_, review)| {
            review.side == side && !review.is_outdated && !is_file_level(review)
        })
        .filter_map(|(thread, review)| Some((review.anchor_line()?, thread)))
        .collect();

    waiting.sort_unstable();
    waiting
}

/// The source line each thread hangs under, by thread index.
///
/// A hunk numbers its lines upward, so one cursor walks the sorted anchors
/// alongside the patch rather than searching it once per thread. The next hunk
/// starts over at its own number, which is what resets the cursor.
fn resolve_anchors(
    file: &ChangedFile,
    threads: &[ReviewThread],
) -> Vec<Option<usize>> {
    let mut anchored: Vec<Option<usize>> = vec![None; threads.len()];

    for side in [Side::Left, Side::Right] {
        let waiting = waiting_on(threads, side);
        if waiting.is_empty() {
            continue;
        }

        let mut cursor = 0;
        for (source, line) in file.lines.iter().enumerate() {
            if line.kind == LineKind::Hunk {
                cursor = 0;
                continue;
            }

            let number = match side {
                Side::Left => line.old_line,
                Side::Right => line.new_line,
            };
            let Some(number) = number else {
                continue;
            };

            while waiting
                .get(cursor)
                .is_some_and(|&(anchor, _)| anchor < number)
            {
                cursor += 1;
            }

            // A number can repeat across hunks, and the row it first appeared
            // on is the one the thread is drawn against.
            while let Some(&(anchor, thread)) = waiting.get(cursor) {
                if anchor != number {
                    break;
                }
                anchored[thread].get_or_insert(source);
                cursor += 1;
            }
        }
    }

    anchored
}

/// Line drafts paired with the row each is drawn under, sorted by that row.
///
/// GitHub anchors a span at its last line, so a note covering several rows sits
/// under the end of what it covers rather than the start.
fn placed_drafts(drafts: &[&Draft]) -> Vec<(usize, usize)> {
    let mut placed: Vec<(usize, usize)> = drafts
        .iter()
        .enumerate()
        .filter_map(|(draft, pending)| Some((*pending.rows()?.end(), draft)))
        .collect();

    placed.sort_unstable();
    placed
}

/// Indices of the threads a predicate picks out, in their original order.
fn select(
    threads: &[ReviewThread],
    is_wanted: impl Fn(&ReviewThread) -> bool,
) -> Vec<usize> {
    threads
        .iter()
        .enumerate()
        .filter(|(_, review)| is_wanted(review))
        .map(|(index, _)| index)
        .collect()
}

pub struct Rows {
    all: Vec<Row>,
    /// Virtual row of each source line, parallel to [`ChangedFile::lines`].
    code: Vec<usize>,
    /// The row each card's own summary is drawn on, for the cursor to follow.
    /// An elided one is absent, since it has no row this frame.
    cards: Vec<(Card, usize)>,
    stops: Vec<Stop>,
    body: Vec<BodyRow>,
    body_limit: usize,
}

impl Rows {
    /// The row list for a pane with no file open.
    pub const fn empty() -> Self {
        Self {
            all: Vec::new(),
            code: Vec::new(),
            cards: Vec::new(),
            stops: Vec::new(),
            body: Vec::new(),
            body_limit: 0,
        }
    }

    pub fn build(
        file: &ChangedFile,
        threads: &[ReviewThread],
        view: View<'_>,
    ) -> Self {
        let mut builder = Builder {
            view,
            threads,
            is_stacked: false,
            rows: Vec::with_capacity(file.lines.len()),
            code: Vec::with_capacity(file.lines.len()),
            cards: Vec::new(),
            body: Vec::new(),
            body_limit: 0,
        };

        // Which line each card hangs under is resolved once, here. The row
        // list groups those stops by line and the cursor walks the same list,
        // so neither side re-derives where a conversation attaches.
        let anchored = resolve_anchors(file, threads);
        let stops = stops_from(file, threads, view.drafts, &anchored);

        // Remarks about the file as a whole answer to no line, so they lead the
        // pane the way GitHub puts them above a file's diff.
        for draft in 0..view.drafts.len() {
            if view.drafts[draft].is_file_level() {
                builder.emit_draft(draft);
            }
        }
        builder.emit_threads(&select(threads, is_file_level));

        // A patch with no lines is still worth opening for its conversations,
        // since there is nothing for any of them to anchor to.
        if file.lines.is_empty() {
            builder.emit_threads(&select(threads, |review| {
                !is_file_level(review)
            }));
            return builder.finish(stops);
        }

        let mut by_source: Vec<Vec<usize>> = vec![Vec::new(); file.lines.len()];
        let mut outdated: Vec<usize> = Vec::new();
        for (thread, review) in threads.iter().enumerate() {
            if is_file_level(review) {
                continue;
            }
            if review.is_outdated {
                outdated.push(thread);
                continue;
            }
            let Some(source) = anchored[thread] else {
                continue;
            };
            by_source[source].push(thread);
        }

        let placed = placed_drafts(view.drafts);
        let last = file.lines.len() - 1;
        for (source, anchored) in by_source.iter().enumerate() {
            builder.emit_gap(source);
            builder.code.push(builder.rows.len());
            builder.emit_code(&file.lines[source], source);
            builder.emit_drafts(&placed, source);
            // Outdated threads have no live anchor, so they pin after the
            // last line. They stack with whatever is already there and share
            // its author column rather than forming a second, narrower one.
            if source == last && !outdated.is_empty() {
                let stack: Vec<usize> =
                    anchored.iter().chain(&outdated).copied().collect();
                builder.emit_threads(&stack);
            } else {
                builder.emit_threads(anchored);
            }
        }

        // The run below the last hunk answers to no line of the patch, so it
        // sits at the foot of the pane where those lines would be.
        builder.emit_gap(file.lines.len());

        builder.finish(stops)
    }

    pub const fn len(&self) -> usize {
        self.all.len()
    }

    pub const fn is_empty(&self) -> bool {
        self.all.is_empty()
    }

    pub fn get(&self, row: usize) -> Option<&Row> {
        self.all.get(row)
    }

    /// The rows visible in a viewport of `height` scrolled to `offset`.
    pub fn window(&self, offset: usize, height: usize) -> &[Row] {
        let start = offset.min(self.all.len().saturating_sub(height));
        let end = start.saturating_add(height).min(self.all.len());

        &self.all[start..end]
    }

    /// The row a source line is drawn on.
    pub fn code_row(&self, source: usize) -> usize {
        self.code.get(source).copied().unwrap_or(0)
    }

    /// The row a card's own summary is drawn on, when it is not elided away.
    pub fn card_row(&self, card: &Card) -> Option<usize> {
        self.cards
            .iter()
            .find(|(held, _)| held == card)
            .map(|(_, row)| *row)
    }

    /// Rows a card occupies: its summary, plus the conversation unfolded under
    /// it while it is the open one.
    pub fn card_height(&self, card: &Card) -> usize {
        let Some(row) = self.card_row(card) else {
            return 0;
        };

        let body = self.all[row + 1..]
            .iter()
            .take_while(|row| matches!(row, Row::Body { .. }))
            .count();

        body + 1
    }

    pub fn body(&self, index: usize) -> Option<&BodyRow> {
        self.body.get(index)
    }

    /// How far the expanded conversation can scroll before it runs out.
    pub const fn body_limit(&self) -> usize {
        self.body_limit
    }

    pub fn stops(&self) -> &[Stop] {
        &self.stops
    }

    /// The threads hanging under one source line, in visit order. Stops are
    /// sorted by source line, so one line's threads are already contiguous.
    pub fn stops_at(&self, source: usize) -> &[Stop] {
        let start = self.stops.partition_point(|stop| stop.source < source);
        let end = self.stops.partition_point(|stop| stop.source <= source);

        &self.stops[start..end]
    }
}

struct Builder<'a> {
    view: View<'a>,
    threads: &'a [ReviewThread],
    /// Whether a card has already been drawn under the line being laid out, so
    /// the next one knows to rule itself off from it.
    is_stacked: bool,
    rows: Vec<Row>,
    code: Vec<usize>,
    cards: Vec<(Card, usize)>,
    body: Vec<BodyRow>,
    body_limit: usize,
}

impl Builder<'_> {
    fn finish(self, stops: Vec<Stop>) -> Rows {
        Rows {
            all: self.rows,
            code: self.code,
            cards: self.cards,
            stops,
            body: self.body,
            body_limit: self.body_limit,
        }
    }

    /// Pushes a card's summary row and remembers where it landed, which is what
    /// the cursor follows and what a scroll measures against.
    ///
    /// Cards stacked under one line are ruled off from each other. The divider
    /// goes in before the row is recorded, so a card still points at its own
    /// summary rather than at the seam above it.
    fn push_card(&mut self, card: Card, row: Row) {
        if self.is_stacked {
            self.rows.push(Row::Divider);
        }
        self.is_stacked = true;

        self.cards.push((card, self.rows.len()));
        self.rows.push(row);
    }

    const fn indent(&self) -> usize {
        let width = self.view.width;
        if GUTTER < width.saturating_sub(1) {
            GUTTER
        } else {
            width.saturating_sub(1)
        }
    }

    const fn card_width(&self) -> usize {
        self.view.width.saturating_sub(self.indent())
    }

    fn is_expanded(&self, thread: usize) -> bool {
        self.view
            .expanded
            .is_some_and(|card| card.is_thread(&self.threads[thread].id))
    }

    /// The run of hidden lines that ends where `source` begins, if one does.
    fn emit_gap(&mut self, source: usize) {
        let Some(gap) = self.view.gaps.iter().position(|gap| gap.at == source)
        else {
            return;
        };

        self.rows.push(Row::Gap { gap });
    }

    /// A line wider than the pane folds onto further rows instead of being cut
    /// off at the edge, where the rest of it could not be read at all.
    fn emit_code(&mut self, line: &DiffLine, source: usize) {
        self.is_stacked = false;

        // Hunk headers are structural and short, and folding one would only
        // break up the rule it draws across the pane.
        if line.kind == LineKind::Hunk {
            self.rows.push(Row::Code {
                source,
                fragment: Fragment::whole(&line.text),
            });
            return;
        }

        let budget = self.view.width.saturating_sub(GUTTER);
        for fragment in wrap::fragments(&line.text, budget) {
            self.rows.push(Row::Code { source, fragment });
        }
    }

    /// The reader's own pending notes on this line. They lead the line's
    /// conversations, since an unsent remark is what is still being worked on.
    fn emit_drafts(&mut self, placed: &[(usize, usize)], source: usize) {
        let start = placed.partition_point(|&(row, _)| row < source);

        for &(row, draft) in &placed[start..] {
            if row != source {
                break;
            }
            self.emit_draft(draft);
        }
    }

    /// One pending note, plus its body when it is open. A draft expands the same
    /// way a thread does: it is the reader's own comment and reads back as one.
    fn emit_draft(&mut self, draft: usize) {
        let view = self.view;
        let Some(pending) = view.drafts.get(draft) else {
            return;
        };

        self.push_card(Card::Draft(pending.id), Row::Draft { draft });

        if !view.expanded.is_some_and(|card| card.is_draft(pending.id)) {
            return;
        }

        let Some(body_width) = self.body_width() else {
            return;
        };

        // No header: the card line right above already says what the note is
        // and how far it has got, and a thread's header is there to name an
        // author the reader does not have.
        let mut content = Vec::new();
        for block in
            markdown::render_blocks(&pending.body, body_width, view.theme)
        {
            match block {
                MarkdownBlock::Text(line) => content.push(ContentRow {
                    spans: line.spans,
                    comment_index: 0,
                    is_header: false,
                }),
                MarkdownBlock::Image { url, alt } => {
                    content.extend(self.image_rows(&url, &alt, 0, body_width));
                }
            }
        }

        self.emit_body(&content, &[]);
    }

    /// Threads sharing a line are drawn as one pile per state, open first.
    /// Every thread hanging under one line, each its own card, in the order
    /// GitHub holds them.
    ///
    /// A settled thread keeps its place rather than sinking below the open
    /// ones: on a single line a resolved question is often the reason the open
    /// thread under it exists, and re-ranking them breaks that reading.
    fn emit_threads(&mut self, group: &[usize]) {
        if group.is_empty() || self.card_width() < 4 {
            return;
        }

        let author_width = self.author_width(group);

        for &thread in group {
            self.push_card(
                Card::Thread(self.threads[thread].id.clone()),
                Row::Summary {
                    thread,
                    state: ThreadState::of(&self.threads[thread]),
                    author_width,
                },
            );
            self.emit_thread_body(thread);
        }
    }

    /// The column every author name in one line's stack is padded to. A single
    /// long name is clipped rather than pushing every summary beside it off the
    /// pane.
    fn author_width(&self, group: &[usize]) -> usize {
        let widest = group
            .iter()
            .map(|&thread| text_width(&author_of(&self.threads[thread])))
            .max()
            .unwrap_or(0);

        widest.min(self.card_width() / 3)
    }

    /// Rows an expanded body has to fit its text into, once the card's frame is
    /// taken off. A pane too narrow for any of it has nothing to draw.
    fn body_width(&self) -> Option<usize> {
        let width = self.card_width().saturating_sub(BODY_INDENT);

        (width > 0).then_some(width)
    }

    fn emit_thread_body(&mut self, thread: usize) {
        if !self.is_expanded(thread) {
            return;
        }

        let Some(body_width) = self.body_width() else {
            return;
        };

        let content = self.content(thread, body_width);
        let continued: Vec<Vec<Span<'static>>> =
            (0..self.threads[thread].comments.len())
                .map(|index| {
                    comment_header(
                        &self.threads[thread],
                        index,
                        self.view.theme,
                        true,
                    )
                })
                .collect();

        self.emit_body(&content, &continued);
    }

    /// Windows a rendered conversation into the rows the pane will draw.
    ///
    /// `continued` carries each comment's header in its continued form, since a
    /// window that opens partway through one has to say whose words these are.
    /// That header costs one row wherever the window opens, which is what keeps
    /// the card the same height at every scroll position: text then moves by
    /// exactly the number of rows the reader asked for, and the diff underneath
    /// holds still.
    fn emit_body(
        &mut self,
        content: &[ContentRow],
        continued: &[Vec<Span<'static>>],
    ) {
        let window = self.view.window;
        let limit = scroll_limit(content, continued, window);
        let start = self.view.scroll.min(limit);
        self.body_limit = limit;

        let mut visible: Vec<BodyRow> = Vec::with_capacity(window);
        let mut cursor = start;

        if start > 0 {
            let row = &content[start];
            if row.is_header {
                visible.push(BodyRow {
                    spans: row.spans.clone(),
                    is_thumb: false,
                });
                cursor += 1;
            } else if let Some(header) = continued.get(row.comment_index) {
                visible.push(BodyRow {
                    spans: header.clone(),
                    is_thumb: false,
                });
            }
        }

        let end = (cursor + window - visible.len()).min(content.len());
        visible.extend(content[cursor..end].iter().map(|row| BodyRow {
            spans: row.spans.clone(),
            is_thumb: false,
        }));

        mark_thumb(&mut visible, start, limit, content.len());

        let base = self.body.len();
        for offset in 0..visible.len() {
            self.rows.push(Row::Body {
                index: base + offset,
            });
        }
        self.body.extend(visible);
    }

    /// The whole conversation as rows, before the scroll window is applied.
    fn content(&self, thread: usize, body_width: usize) -> Vec<ContentRow> {
        let review = &self.threads[thread];
        let theme = self.view.theme;
        let mut content = Vec::new();

        for (comment_index, comment) in review.comments.iter().enumerate() {
            // Replies run straight into the comment above them otherwise, and a
            // long exchange reads as one block of text rather than a back and
            // forth. A blank row is not enough on its own: beside the card's
            // own background it reads as nothing at all.
            //
            // The seam is inset behind the rail and drawn back, so it stays
            // plainly weaker than the rule between two whole threads.
            if comment_index > 0 {
                content.push(ContentRow {
                    spans: vec![card_span(
                        "┄".repeat(body_width),
                        theme.dim,
                        Modifier::empty(),
                        theme,
                    )],
                    comment_index,
                    is_header: true,
                });
            }

            content.push(ContentRow {
                spans: comment_header(review, comment_index, theme, false),
                comment_index,
                is_header: true,
            });

            for block in
                markdown::render_blocks(&comment.body, body_width, theme)
            {
                match block {
                    MarkdownBlock::Text(line) => content.push(ContentRow {
                        spans: line.spans,
                        comment_index,
                        is_header: false,
                    }),
                    MarkdownBlock::Image { url, alt } => {
                        content.extend(self.image_rows(
                            &url,
                            &alt,
                            comment_index,
                            body_width,
                        ));
                    }
                }
            }
        }

        if content.is_empty() {
            content.push(ContentRow {
                spans: vec![card_span(
                    "no comments",
                    theme.muted,
                    Modifier::empty(),
                    theme,
                )],
                comment_index: 0,
                is_header: true,
            });
        }

        content
    }

    /// An image reads as the link it was: its alt text, or the URL when it
    /// carries none.
    fn image_rows(
        &self,
        url: &str,
        alt: &str,
        comment_index: usize,
        body_width: usize,
    ) -> Vec<ContentRow> {
        markdown::image_lines(url, alt, None, body_width, self.view.theme)
            .into_iter()
            .map(|line| ContentRow {
                spans: line.spans,
                comment_index,
                is_header: false,
            })
            .collect()
    }
}

/// A conversation row before windowing. `comment_index` and `is_header` only
/// matter while deciding whether a scrolled window needs a continued header.
struct ContentRow {
    spans: Vec<Span<'static>>,
    comment_index: usize,
    is_header: bool,
}

/// Rows an expanded conversation may occupy before it starts scrolling: about
/// two thirds of the pane, but never so much that the diff disappears.
pub fn thread_window(height: usize) -> usize {
    (height.saturating_mul(2) / 3)
        .max(1)
        .min(height.saturating_sub(6).max(1))
}

/// The furthest the window may start and still reach the conversation's last
/// line. A window that has to reprint a header gives up one row of text for it,
/// so the plain arithmetic falls one short.
fn scroll_limit(
    content: &[ContentRow],
    continued: &[Vec<Span<'static>>],
    window: usize,
) -> usize {
    let plain = content.len().saturating_sub(window);
    if plain == 0 {
        return 0;
    }

    let row = &content[plain];
    if row.is_header || continued.get(row.comment_index).is_none() {
        return plain;
    }

    plain + 1
}

/// Marks the run of rows the scrollbar thumb covers. A conversation that fits
/// its window has nothing to mark, which is what makes the bright run mean
/// "there is more of this".
fn mark_thumb(
    visible: &mut [BodyRow],
    start: usize,
    limit: usize,
    total: usize,
) {
    if limit == 0 || visible.is_empty() {
        return;
    }

    let height = visible.len();
    let thumb = (height * height / total).clamp(1, height);
    let top = start * (height - thumb) / limit;

    for row in &mut visible[top..(top + thumb).min(height)] {
        row.is_thumb = true;
    }
}

/// How a thread names itself in a summary: whoever opened it.
pub fn author_of(thread: &ReviewThread) -> String {
    thread.comments.first().map_or_else(
        || "review thread".to_string(),
        |comment| format!("@{}", comment.author),
    )
}

fn comment_header(
    thread: &ReviewThread,
    comment_index: usize,
    theme: Theme,
    continued: bool,
) -> Vec<Span<'static>> {
    let Some(comment) = thread.comments.get(comment_index) else {
        return Vec::new();
    };

    let date = comment.created_at.get(..10).unwrap_or(&comment.created_at);
    let mut header = if comment_index == 0 {
        format!("@{}", comment.author)
    } else {
        format!("↳ @{}", comment.author)
    };

    if !date.is_empty() {
        header.push_str(" · ");
        header.push_str(date);
    }
    if continued {
        header.push_str(" · continued");
    }

    vec![card_span(header, theme.heading, Modifier::BOLD, theme)]
}

/// Thread cards sit on their own background, so every span inside one carries it
/// rather than leaving gaps where the diff shows through.
pub fn card_span(
    text: impl Into<Cow<'static, str>>,
    color: Color,
    modifier: Modifier,
    theme: Theme,
) -> Span<'static> {
    Span::styled(
        text,
        Style::default()
            .bg(theme.hunk)
            .fg(color)
            .add_modifier(modifier),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn line(kind: LineKind, old: Option<u32>, new: Option<u32>) -> DiffLine {
        DiffLine {
            kind,
            text: "code".into(),
            old_line: old,
            new_line: new,
        }
    }

    fn patch(lines: Vec<DiffLine>) -> ChangedFile {
        ChangedFile {
            path: Arc::from("a.rs"),
            status: "modified".into(),
            additions: 0,
            deletions: 0,
            lines,
        }
    }

    fn thread(id: &str, side: Side, anchor: u32) -> ReviewThread {
        ReviewThread {
            id: Arc::from(id),
            path: Arc::from("a.rs"),
            line: Some(anchor),
            original_line: Some(anchor),
            start_line: None,
            side,
            start_side: None,
            is_file_level: false,
            is_resolved: false,
            is_outdated: false,
            can_resolve: true,
            comments: Vec::new(),
        }
    }

    fn stops(
        file: &ChangedFile,
        threads: &[ReviewThread],
    ) -> Vec<(usize, Card)> {
        stops_for(file, threads, &[])
            .into_iter()
            .map(|stop| (stop.source, stop.card))
            .collect()
    }

    fn comment(author: &str, body: &str) -> prtui_core::Comment {
        prtui_core::Comment {
            id: Arc::from("C_1"),
            reply_target: None,
            author: author.into(),
            body: body.into(),
            created_at: "2024-04-29T14:06:54Z".into(),
            is_pending: false,
        }
    }

    /// A card that grows or shrinks as the reader scrolls shifts the diff under
    /// it on every keystroke, and the text it holds moves by something other
    /// than what was asked for.
    #[test]
    fn an_open_conversation_keeps_its_height_at_every_scroll_position() {
        let file = patch(vec![
            line(LineKind::Hunk, None, None),
            line(LineKind::Context, Some(1), Some(1)),
        ]);
        let mut review = thread("t", Side::Right, 1);
        review.comments = (0..6)
            .map(|index| {
                comment("williammartin", &format!("Comment {index} body text"))
            })
            .collect();

        let card = Card::Thread(review.id.clone());
        let window = 8;
        let mut heights = Vec::new();
        let mut limit;
        let mut scroll = 0;

        loop {
            let rows = Rows::build(
                &file,
                std::slice::from_ref(&review),
                View {
                    focused: Some(&card),
                    expanded: Some(&card),
                    scroll,
                    width: 80,
                    window,
                    theme: Theme::dark(),
                    drafts: &[],
                    gaps: &[],
                },
            );

            heights.push(
                (0..rows.len())
                    .filter(|&row| {
                        matches!(rows.get(row), Some(Row::Body { .. }))
                    })
                    .count(),
            );
            limit = rows.body_limit();

            if scroll >= limit {
                break;
            }
            scroll += 1;
        }

        assert!(limit > 0, "the fixture has to overflow its window");
        assert_eq!(heights, vec![window; heights.len()]);
    }

    #[test]
    fn an_anchor_resolves_against_its_own_side() {
        // A removed line carries only an old number, an added one only a new.
        let file = patch(vec![
            line(LineKind::Hunk, None, None),
            line(LineKind::Removed, Some(10), None),
            line(LineKind::Added, None, Some(10)),
        ]);
        let threads = vec![
            thread("old", Side::Left, 10),
            thread("new", Side::Right, 10),
        ];

        assert_eq!(
            stops(&file, &threads),
            [
                (1, Card::Thread(Arc::from("old"))),
                (2, Card::Thread(Arc::from("new"))),
            ]
        );
    }

    #[test]
    fn a_later_hunk_resolves_from_its_own_numbers() {
        let file = patch(vec![
            line(LineKind::Hunk, None, None),
            line(LineKind::Context, Some(1), Some(1)),
            line(LineKind::Hunk, None, None),
            line(LineKind::Context, Some(80), Some(80)),
            line(LineKind::Context, Some(81), Some(81)),
        ]);
        let threads = vec![
            thread("late", Side::Right, 81),
            thread("early", Side::Right, 1),
            thread("absent", Side::Right, 40),
        ];

        // The walk leaves 81 behind while it is inside the first hunk, so the
        // second has to start over. An anchor no line carries is dropped:
        // there is no row to reach it from.
        assert_eq!(
            stops(&file, &threads),
            [
                (1, Card::Thread(Arc::from("early"))),
                (4, Card::Thread(Arc::from("late"))),
            ]
        );
    }

    fn settled(mut review: ReviewThread, state: ThreadState) -> ReviewThread {
        review.is_resolved = state == ThreadState::Resolved;
        review.is_outdated = state == ThreadState::Outdated;
        review
    }

    /// Threads sharing a line keep the order GitHub holds them in. Ranking the
    /// open ones to the top reads the exchange out of sequence, since a settled
    /// thread is often why the open one under it exists.
    #[test]
    fn threads_on_one_line_keep_the_order_github_gave_them() {
        let file = patch(vec![
            line(LineKind::Hunk, None, None),
            line(LineKind::Context, Some(1), Some(1)),
        ]);
        let threads = vec![
            thread("first", Side::Right, 1),
            settled(thread("second", Side::Right, 1), ThreadState::Resolved),
            thread("third", Side::Right, 1),
        ];

        assert_eq!(
            stops(&file, &threads),
            [
                (1, Card::Thread(Arc::from("first"))),
                (1, Card::Thread(Arc::from("second"))),
                (1, Card::Thread(Arc::from("third"))),
            ]
        );
    }

    /// The cursor walks cards in the order the pane draws them. Both orders are
    /// derived separately, so nothing but a test keeps them from drifting apart
    /// and leaving `j` jumping backwards up the pane.
    #[test]
    fn the_cursor_walks_cards_in_the_order_the_pane_draws_them() {
        let file = patch(vec![
            line(LineKind::Hunk, None, None),
            line(LineKind::Context, Some(1), Some(1)),
            line(LineKind::Context, Some(2), Some(2)),
        ]);
        let threads = vec![
            thread("live-b", Side::Right, 2),
            settled(thread("stale", Side::Right, 2), ThreadState::Outdated),
            settled(thread("done", Side::Right, 1), ThreadState::Resolved),
            thread("live-a", Side::Right, 1),
        ];

        let rows = Rows::build(
            &file,
            &threads,
            View {
                focused: None,
                expanded: None,
                scroll: 0,
                width: 80,
                window: 8,
                theme: Theme::dark(),
                drafts: &[],
                gaps: &[],
            },
        );

        let drawn: Vec<usize> = rows
            .stops()
            .iter()
            .map(|stop| {
                rows.card_row(&stop.card)
                    .expect("every stop has a row to reach it from")
            })
            .collect();

        assert_eq!(drawn.len(), threads.len());
        assert!(
            drawn.windows(2).all(|pair| pair[0] < pair[1]),
            "stops {drawn:?} are not in drawing order"
        );
    }

    #[test]
    fn a_number_repeated_across_hunks_keeps_its_first_row() {
        let file = patch(vec![
            line(LineKind::Hunk, None, None),
            line(LineKind::Context, Some(5), Some(5)),
            line(LineKind::Hunk, None, None),
            line(LineKind::Context, Some(5), Some(5)),
        ]);

        assert_eq!(
            stops(&file, &[thread("t", Side::Right, 5)]),
            [(1, Card::Thread(Arc::from("t")))]
        );
    }

    /// The reader's own notes lead the conversations on their line, and a
    /// remark about the whole file leads the pane, which is where each is
    /// drawn.
    #[test]
    fn drafts_are_stops_ahead_of_the_threads_they_share_a_line_with() {
        let file = patch(vec![
            line(LineKind::Hunk, None, None),
            line(LineKind::Context, Some(1), Some(1)),
        ]);
        let threads = vec![thread("t", Side::Right, 1)];
        let note = Draft {
            id: 7,
            path: Arc::from("a.rs"),
            attachment: crate::app::draft::Attachment::File,
            body: "about the file".into(),
            remote: None,
            sync: crate::app::draft::Sync::Synced,
        };
        let inline = Draft {
            id: 9,
            attachment: crate::app::draft::Attachment::Lines {
                rows: 1..=1,
                anchor: crate::app::draft::Anchor {
                    start_line: 1,
                    start_side: Side::Right,
                    end_line: 1,
                    side: Side::Right,
                },
            },
            ..note.clone()
        };

        assert_eq!(
            stops_for(&file, &threads, &[&note, &inline])
                .into_iter()
                .map(|stop| (stop.source, stop.card))
                .collect::<Vec<_>>(),
            [
                (0, Card::Draft(7)),
                (1, Card::Draft(9)),
                (1, Card::Thread(Arc::from("t"))),
            ]
        );
    }
}
