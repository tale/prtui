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

use crate::model::{ChangedFile, DiffLine, LineKind, ReviewThread};
use crate::renderer::Theme;
use crate::renderer::markdown::{self, Block as MarkdownBlock};
use crate::text::wrap::{self, Fragment};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use std::borrow::Cow;
use std::fmt::Write;

/// Columns the line-number gutter occupies, which is also how far a thread card
/// is indented so it hangs under the code rather than beside it.
pub const GUTTER: usize = 13;

/// Summaries shown at once when several threads share a line; the rest elide.
const MAX_VISIBLE_SUMMARIES: usize = 4;

/// Which pile a thread sits in. Open threads are what review is about, so they
/// sort and render ahead of the settled ones.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadState {
    Open,
    Resolved,
    Outdated,
}

impl ThreadState {
    pub const ALL: [Self; 3] = [Self::Open, Self::Resolved, Self::Outdated];

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

    const fn rank(self) -> u8 {
        match self {
            Self::Open => 0,
            Self::Resolved => 1,
            Self::Outdated => 2,
        }
    }
}

/// How a summary attaches to the group above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Connector {
    /// The only thread in its pile, so it carries the pile's own marker.
    Only,
    Branch,
    Last,
}

/// One row of the diff pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Row {
    /// One row's worth of a line of the patch. A line too wide for the pane
    /// folds across several of these, all naming the same `source`.
    Code { source: usize, fragment: Fragment },
    /// The file-level draft, which answers to no line and leads the pane.
    FileDraft,
    /// A pile's heading, drawn only when the pile holds more than one thread.
    Heading { state: ThreadState, count: usize },
    /// Stands in for the summaries the window left out: `… n earlier` above
    /// the visible ones, `… n more` below them.
    Hidden {
        state: ThreadState,
        count: usize,
        is_tail: bool,
    },
    /// A thread's one-line summary, by index into the file's thread slice.
    Summary {
        thread: usize,
        state: ThreadState,
        connector: Connector,
        /// A lone thread names its own state; a pile's heading already did.
        has_state_label: bool,
    },
    /// One line of the expanded conversation, by index into [`Rows::body`].
    Body { index: usize, is_last: bool },
}

/// A rendered line of an expanded conversation.
#[derive(Debug, Clone)]
pub struct BodyRow {
    pub spans: Vec<Span<'static>>,
}

/// A thread paired with the source line it hangs under, in the order the cursor
/// visits them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stop {
    pub source: usize,
    pub thread: usize,
}

/// What laying a file out needs from app state.
#[derive(Clone, Copy)]
pub struct View<'a> {
    pub focused: Option<&'a str>,
    pub expanded: Option<&'a str>,
    /// How far into the expanded conversation the reader has scrolled.
    pub scroll: usize,
    pub width: usize,
    /// Rows the expanded conversation may occupy before it scrolls.
    pub window: usize,
    pub theme: Theme,
    /// Body of the file-level draft on this file, if one is pending.
    pub file_draft: Option<&'a str>,
}

/// A conversation about the file rather than any line in it.
///
/// GitHub leaves both line numbers null for those. An outdated thread also loses
/// `line`, but keeps `originalLine`, which is what tells the two apart.
pub fn is_file_level(thread: &ReviewThread) -> bool {
    !thread.is_outdated && thread.anchor_line().is_none()
}

/// Every thread in `file` paired with the line it hangs under, in visit order.
///
/// File-level threads belong to no line and lead the pane, so they pin to the
/// first. Outdated ones have no live anchor and pin after the last, matching
/// where each is drawn. A thread whose anchor is not in the patch at all is
/// dropped: there is no row to reach it from.
pub fn stops_for(file: &ChangedFile, threads: &[ReviewThread]) -> Vec<Stop> {
    let last = file.lines.len().saturating_sub(1);
    let mut stops: Vec<Stop> = threads
        .iter()
        .enumerate()
        .filter_map(|(thread, review)| {
            let source = if is_file_level(review) {
                0
            } else if review.is_outdated {
                last
            } else {
                file.lines.iter().position(|line| review.anchors_to(line))?
            };
            Some(Stop { source, thread })
        })
        .collect();

    stops.sort_by_key(|stop| {
        let review = &threads[stop.thread];
        (
            stop.source,
            u8::from(!is_file_level(review)),
            ThreadState::of(review).rank(),
        )
    });
    stops
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
            rows: Vec::with_capacity(file.lines.len()),
            code: Vec::with_capacity(file.lines.len()),
            body: Vec::new(),
            body_limit: 0,
        };

        // Remarks about the file as a whole answer to no line, so they lead the
        // pane the way GitHub puts them above a file's diff.
        if builder.view.file_draft.is_some() {
            builder.rows.push(Row::FileDraft);
        }
        builder.emit_piles(&select(threads, is_file_level));

        // A patch with no lines is still worth opening for its conversations,
        // since there is nothing for any of them to anchor to.
        if file.lines.is_empty() {
            builder
                .emit_piles(&select(threads, |review| !is_file_level(review)));
            return builder.finish(stops_for(file, threads));
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
            if let Some(source) =
                file.lines.iter().position(|line| review.anchors_to(line))
            {
                by_source[source].push(thread);
            }
        }

        let last = file.lines.len() - 1;
        for (source, anchored) in by_source.iter().enumerate() {
            builder.code.push(builder.rows.len());
            builder.emit_code(&file.lines[source], source);
            builder.emit_piles(anchored);

            if source == last {
                builder.emit_piles(&outdated);
            }
        }

        builder.finish(stops_for(file, threads))
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

    /// The row a thread's summary is drawn on, when it is not elided away.
    pub fn summary_row(&self, thread: usize) -> Option<usize> {
        self.all.iter().position(|row| {
            matches!(row, Row::Summary { thread: found, .. } if *found == thread)
        })
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
    rows: Vec<Row>,
    code: Vec<usize>,
    body: Vec<BodyRow>,
    body_limit: usize,
}

impl Builder<'_> {
    fn finish(self, stops: Vec<Stop>) -> Rows {
        Rows {
            all: self.rows,
            code: self.code,
            stops,
            body: self.body,
            body_limit: self.body_limit,
        }
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

    fn is_focused(&self, thread: usize) -> bool {
        self.view.focused == Some(self.threads[thread].id.as_str())
    }

    fn is_expanded(&self, thread: usize) -> bool {
        self.view.expanded == Some(self.threads[thread].id.as_str())
    }

    /// A line wider than the pane folds onto further rows instead of being cut
    /// off at the edge, where the rest of it could not be read at all.
    fn emit_code(&mut self, line: &DiffLine, source: usize) {
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

    /// Threads sharing a line are drawn as one pile per state, open first.
    fn emit_piles(&mut self, group: &[usize]) {
        for state in ThreadState::ALL {
            let pile: Vec<usize> = group
                .iter()
                .copied()
                .filter(|&thread| {
                    ThreadState::of(&self.threads[thread]) == state
                })
                .collect();
            self.emit_pile(state, &pile);
        }
    }

    fn emit_pile(&mut self, state: ThreadState, pile: &[usize]) {
        if pile.is_empty() || self.card_width() < 4 {
            return;
        }

        if let [only] = *pile {
            self.rows.push(Row::Summary {
                thread: only,
                state,
                connector: Connector::Only,
                has_state_label: true,
            });
            self.emit_body(only);
            return;
        }

        let count = pile.len();
        self.rows.push(Row::Heading { state, count });

        let start = self.window_start(pile);
        let end = (start + MAX_VISIBLE_SUMMARIES).min(count);

        if start > 0 {
            self.rows.push(Row::Hidden {
                state,
                count: start,
                is_tail: false,
            });
        }

        for (position, &thread) in pile.iter().enumerate().take(end).skip(start)
        {
            self.rows.push(Row::Summary {
                thread,
                state,
                connector: if position + 1 == count {
                    Connector::Last
                } else {
                    Connector::Branch
                },
                has_state_label: false,
            });
            self.emit_body(thread);
        }

        if end < count {
            self.rows.push(Row::Hidden {
                state,
                count: count - end,
                is_tail: true,
            });
        }
    }

    /// An expanded thread has to be on screen; otherwise the window follows the
    /// focused summary, keeping it at the bottom as the cursor walks down.
    fn window_start(&self, pile: &[usize]) -> usize {
        if let Some(position) =
            pile.iter().position(|&thread| self.is_expanded(thread))
        {
            return position;
        }

        pile.iter()
            .position(|&thread| self.is_focused(thread))
            .map_or(0, |position| {
                position.saturating_sub(MAX_VISIBLE_SUMMARIES - 1)
            })
            .min(pile.len().saturating_sub(MAX_VISIBLE_SUMMARIES))
    }

    fn emit_body(&mut self, thread: usize) {
        if !self.is_expanded(thread) {
            return;
        }

        let body_width = self.card_width().saturating_sub(3);
        if body_width == 0 {
            return;
        }

        let content = self.content(thread, body_width);
        let window = self.view.window;
        let start = self.view.scroll.min(content.len().saturating_sub(window));
        let end = (start + window).min(content.len());
        self.body_limit = content.len().saturating_sub(window);

        let theme = self.view.theme;
        let mut visible: Vec<BodyRow> = Vec::new();

        if start > 0 {
            visible.push(note(format!("↑ {start} earlier"), theme));

            // Scrolling into the middle of a comment loses the header that says
            // whose it is, so it is reprinted as continued.
            if let Some(row) = content.get(start)
                && !row.is_header
            {
                visible.push(BodyRow {
                    spans: comment_header(
                        &self.threads[thread],
                        row.comment_index,
                        theme,
                        true,
                    ),
                });
            }
        }

        visible.extend(content[start..end].iter().map(|row| BodyRow {
            spans: row.spans.clone(),
        }));

        if end < content.len() {
            visible
                .push(note(format!("↓ {} more", content.len() - end), theme));
        }

        let base = self.body.len();
        let last = visible.len().saturating_sub(1);

        for offset in 0..visible.len() {
            self.rows.push(Row::Body {
                index: base + offset,
                is_last: offset == last,
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

fn note(text: String, theme: Theme) -> BodyRow {
    BodyRow {
        spans: vec![card_span(text, theme.muted, Modifier::empty(), theme)],
    }
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
    if comment_index > 0 {
        let replies = thread.comments.len().saturating_sub(1);
        let _ = write!(header, " · reply {comment_index}/{replies}");
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
