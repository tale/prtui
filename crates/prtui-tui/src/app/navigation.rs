//! Cursor, pane, file, and conversation navigation.

use super::action::Motion;
use super::{App, Card, Draft, Mode, Pane};
use crate::layout::rows::Rows;
use crate::layout::tree::Row as TreeNode;
use crate::layout::{Layout, rows};
use crate::vim::step;

impl App {
    pub(super) fn toggle_pane(&mut self) {
        if self.navigation.mode != Mode::Normal {
            return;
        }

        if self.navigation.pane == Pane::Files {
            self.focus_diff();
        } else {
            self.focus_files();
        }
    }

    pub(super) fn activate(&mut self, layout: &Layout) {
        if self.navigation.pane == Pane::Files {
            // A heading has nothing to open, so the same key folds it.
            if self.navigation.tree_directory.is_some() {
                self.toggle_directory(layout);
            } else {
                self.focus_diff();
            }
            return;
        }

        let Some(card) = self.navigation.focused_card.as_ref() else {
            return;
        };
        self.navigation.thread_scroll = 0;

        if self.navigation.expanded_card.as_ref() == Some(card) {
            self.navigation.expanded_card = None;
            return;
        }

        self.navigation.expanded_card = Some(card.clone());

        // The conversation this just unfolded is not in the drawn row list, so
        // the room it needs has to be measured against a fresh one.
        let rows = layout.rebuild_rows(self.view());
        self.reveal_card(&rows, layout.diff_viewport());
    }

    pub(super) fn focus_files(&mut self) {
        if self.navigation.mode != Mode::Normal {
            return;
        }

        self.navigation.is_files_visible = true;
        self.navigation.pane = Pane::Files;
    }

    pub(super) fn focus_diff(&mut self) {
        if self.navigation.mode == Mode::Normal {
            self.navigation.pane = Pane::Diff;
        }
    }

    pub(super) fn select_file(&mut self, index: usize) {
        self.set_selected_file(index, true);
    }

    /// `]` and `[` walk files only, skipping the headings `j` stops on, and in
    /// the order the tree lists them rather than the order GitHub sent them.
    ///
    /// The list is a ring: the file after the last one is the first. A review
    /// is read in laps, and a reader who starts in the middle of the tree
    /// would otherwise have to turn round to see what is above them.
    pub(super) fn step_file(
        &mut self,
        direction: isize,
        count: usize,
        layout: &Layout,
    ) {
        let files: Vec<usize> = layout.files.files().collect();
        let Some(position) = files
            .iter()
            .position(|&index| index == self.navigation.selected_file)
        else {
            return;
        };

        // A count larger than the tree would only lap it.
        let stride = count % files.len();
        let steps = direction.saturating_mul(stride.cast_signed());
        let target = (position.cast_signed() + steps)
            .rem_euclid(files.len().cast_signed())
            as usize;

        let is_forwards = direction > 0;
        let wrapped = stride != 0
            && if is_forwards {
                position + stride >= files.len()
            } else {
                stride > position
            };

        self.select_file(files[target]);
        self.runtime.status.clear();

        if wrapped {
            self.note_wrap(is_forwards);
        }
    }

    /// The visible files in tree order, starting after the open one and coming
    /// back round to it. This is the order every file-level jump searches in,
    /// which is what makes a stop above the cursor reachable by walking on.
    fn file_ring(&self, direction: isize, layout: &Layout) -> Vec<usize> {
        let files: Vec<usize> = layout.files.files().collect();
        let Some(position) = files
            .iter()
            .position(|&index| index == self.navigation.selected_file)
        else {
            return files;
        };

        let mut ring: Vec<usize> = files[position + 1..]
            .iter()
            .chain(&files[..position])
            .copied()
            .collect();

        // Reversing "everything below, then everything above" gives
        // "everything above, then everything below", each walked upward.
        if direction < 0 {
            ring.reverse();
        }

        ring
    }

    /// Whether the file is one the reader has not marked read through.
    fn is_unread(&self, index: usize) -> bool {
        self.review
            .files
            .get(index)
            .is_some_and(|file| !self.review.viewed.contains(&file.path))
    }

    /// The next file the reader has not been through, in the order the tree
    /// lists them, coming back round to the ones above.
    ///
    /// Files already marked are stepped over rather than landed on. `x` on a
    /// marked file clears its mark, so stopping there would turn a walk down
    /// the review into undoing the last session's work.
    pub(super) fn unread_after_current(
        &self,
        layout: &Layout,
    ) -> Option<usize> {
        self.file_ring(1, layout)
            .into_iter()
            .find(|&index| self.is_unread(index))
    }

    /// Says that the jump came back round, the way `/` says a search did.
    /// A jump that moves the reader somewhere they did not expect has to
    /// account for itself.
    fn note_wrap(&mut self, is_forwards: bool) {
        self.runtime.status = if is_forwards {
            "wrapped to the top".into()
        } else {
            "wrapped to the bottom".into()
        };
    }

    pub(super) fn set_selected_file(
        &mut self,
        index: usize,
        leave_transient_mode: bool,
    ) {
        if self.review.files.is_empty() {
            return;
        }

        self.navigation.selected_file = index.min(self.review.files.len() - 1);
        self.navigation.tree_directory = None;
        self.navigation.cursor = 0;
        self.set_focus(None);
        self.navigation.diff_scroll = 0;
        self.navigation.selection = None;
        if leave_transient_mode {
            self.navigation.mode = Mode::Normal;
        }
    }

    pub(super) fn travel(&mut self, motion: Motion, layout: &Layout) {
        if self.navigation.mode.is_overlay() {
            self.navigation.overlay.apply(
                motion,
                layout.overlay_len(),
                layout.overlay_viewport(),
            );
            return;
        }

        if self.navigation.pane == Pane::Files {
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
        if self.navigation.mode != Mode::Visual
            && self.navigation.expanded_card.is_some()
            && !matches!(motion, Motion::Line(_))
        {
            let limit = layout.rows.body_limit();
            let target = step(
                motion,
                self.navigation.thread_scroll,
                limit + 1,
                viewport,
            );

            // `gg` and `G` mean the ends of the conversation, not the ends of
            // the file, so they stay inside it even when nothing moves.
            if target != self.navigation.thread_scroll
                || matches!(motion, Motion::Top | Motion::Bottom)
            {
                self.navigation.thread_scroll = target;
                return;
            }
        }

        if self.navigation.mode == Mode::Visual {
            self.navigation.cursor = match motion {
                Motion::Line(number) => self.row_of_line(number),
                _ => step(
                    motion,
                    self.navigation.cursor,
                    self.diff_len(),
                    viewport,
                ),
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
                    self.navigation.cursor = 0;
                    self.set_focus(None);
                }
                Motion::Bottom => {
                    self.navigation.cursor = self.diff_len().saturating_sub(1);
                    self.set_focus(None);
                }
                Motion::Line(number) => {
                    self.navigation.cursor = self.row_of_line(number);
                    self.set_focus(None);
                }
            }
        }

        if let Some(selection) = &mut self.navigation.selection {
            selection.head = self.navigation.cursor;
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
                self.navigation.tree_directory = None;
                self.set_selected_file(*index, false);
            }
            Some(TreeNode::Directory { path, .. }) => {
                self.navigation.tree_directory = Some(path.clone());
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
        self.navigation
            .tree_directory
            .as_deref()
            .map_or_else(
                || layout.files.row_of(self.navigation.selected_file),
                |path| layout.files.row_of_directory(path),
            )
            .unwrap_or(0)
    }

    /// Folds the heading the cursor is on, or the one the open file sits under.
    /// Unfolding leaves the cursor where it is, so the contents appear below it.
    pub(super) fn toggle_directory(&mut self, layout: &Layout) {
        let mut path = self.navigation.tree_directory.clone();

        if path.is_none() {
            let row = self.tree_cursor(layout);
            // Folding away the file the cursor is on would strand it, so the
            // cursor moves up to the heading swallowing it.
            path = layout.files.enclosing_directory(row).cloned();
            self.navigation.tree_directory.clone_from(&path);
        }

        let Some(path) = path else {
            return;
        };

        if !self.navigation.collapsed.remove(&path) {
            self.navigation.collapsed.insert(path);
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
        let stops = layout.rows.stops_at(self.navigation.cursor);

        if direction > 0 {
            if let Some(focused) = self.navigation.focused_card.as_ref() {
                if let Some(position) =
                    stops.iter().position(|stop| stop.card == *focused)
                    && let Some(next) = stops.get(position + 1)
                {
                    self.set_focus(Some(next.card.clone()));
                    return true;
                }
                if self.navigation.cursor + 1 < self.diff_len() {
                    self.navigation.cursor += 1;
                    self.set_focus(None);
                    return true;
                }
                return false;
            }

            if let Some(first) = stops.first() {
                self.set_focus(Some(first.card.clone()));
                return true;
            }
            if self.navigation.cursor + 1 < self.diff_len() {
                self.navigation.cursor += 1;
                return true;
            }
            return false;
        }

        if let Some(focused) = self.navigation.focused_card.as_ref() {
            if let Some(position) =
                stops.iter().position(|stop| stop.card == *focused)
            {
                if position > 0 {
                    self.set_focus(Some(stops[position - 1].card.clone()));
                } else {
                    self.set_focus(None);
                }
                return true;
            }
            self.set_focus(None);
            return true;
        }

        if self.navigation.cursor == 0 {
            return false;
        }
        self.navigation.cursor -= 1;
        let previous = layout.rows.stops_at(self.navigation.cursor);
        self.set_focus(previous.last().map(|stop| stop.card.clone()));
        true
    }

    pub(super) fn jump_comment(
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

        let is_forwards = direction > 0;
        let from = layout.files.file_position(self.navigation.selected_file);

        if let Some((index, row, card)) =
            self.comment_stop_elsewhere(direction, layout)
        {
            let to = layout.files.file_position(index);
            self.set_selected_file(index, false);
            self.land_on(row, Some(card), layout);

            if if is_forwards { to <= from } else { to >= from } {
                self.note_wrap(is_forwards);
            }
            return true;
        }

        // All the way round the ring and back into the file it started in: the
        // last conversation in a review still steps to the first.
        let stops = self.comment_stops(self.navigation.selected_file);
        let Some((row, card)) = if is_forwards {
            stops.first()
        } else {
            stops.last()
        }
        .cloned() else {
            self.runtime.status = "no more comments".into();
            return false;
        };

        self.land_on(row, Some(card), layout);
        self.note_wrap(is_forwards);
        true
    }

    /// Puts the cursor on a diff row, optionally focusing one of its cards,
    /// and scrolls the row into view.
    pub(super) fn land_on(
        &mut self,
        row: usize,
        card: Option<Card>,
        layout: &Layout,
    ) {
        self.navigation.pane = Pane::Diff;
        self.navigation.selection = None;
        self.navigation.cursor = row;
        self.set_focus(card);

        // A landing may have opened another file, whose rows the drawn layout
        // knows nothing about.
        let rows = layout.rebuild_rows(self.view());
        self.reveal_card(&rows, layout.diff_viewport());
        self.runtime.status.clear();
    }

    /// The subset of cards that `}` and `{` stop at. Jumps cross files, so
    /// this anchors a file the layout has not laid out.
    ///
    /// A settled conversation is not what a review is being read for, so
    /// resolved and outdated threads are skipped. Every draft is a stop: an
    /// unsent remark is the one thing still waiting on the reader.
    fn comment_stops(&self, index: usize) -> Vec<(usize, Card)> {
        let Some(file) = self.review.files.get(index) else {
            return Vec::new();
        };
        let threads = self
            .review
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
    pub(super) fn drafts_for(&self, path: &str) -> Vec<&Draft> {
        self.review
            .drafts
            .iter()
            .filter(|draft| *draft.path == *path)
            .collect()
    }

    /// A focused card that is not itself a stop (resolved or outdated) falls
    /// back to the cursor row, so the jump still moves in the right direction.
    fn comment_stop_here(&self, direction: isize) -> Option<(usize, Card)> {
        let stops = self.comment_stops(self.navigation.selected_file);
        let current =
            self.navigation.focused_card.as_ref().and_then(|focused| {
                stops.iter().position(|(_, card)| card == focused)
            });

        let target = match (current, direction > 0) {
            (Some(index), true) => (index + 1 < stops.len()).then(|| index + 1),
            (Some(index), false) => index.checked_sub(1),
            (None, true) => stops
                .iter()
                .position(|(row, _)| *row >= self.navigation.cursor),
            (None, false) => stops
                .iter()
                .rposition(|(row, _)| *row <= self.navigation.cursor),
        }?;

        Some(stops[target].clone())
    }

    /// The next stop outside the open file, searched round the ring: the
    /// conversations above the cursor come after the ones below it rather than
    /// being out of reach.
    fn comment_stop_elsewhere(
        &self,
        direction: isize,
        layout: &Layout,
    ) -> Option<(usize, usize, Card)> {
        self.comment_stop_in(
            self.file_ring(direction, layout).into_iter(),
            direction > 0,
        )
    }

    fn comment_stop_in(
        &self,
        mut files: impl Iterator<Item = usize>,
        forwards: bool,
    ) -> Option<(usize, usize, Card)> {
        files.find_map(|index| {
            let stops = self.comment_stops(index);
            let stop = if forwards {
                stops.first()
            } else {
                stops.last()
            }?;

            Some((index, stop.0, stop.1.clone()))
        })
    }

    pub(super) fn set_focus(&mut self, focused: Option<Card>) {
        if self.navigation.focused_card != focused {
            self.navigation.expanded_card = None;
            self.navigation.thread_scroll = 0;
        }
        self.navigation.focused_card = focused;
    }

    /// The focused thread, when the focus is on one rather than on a draft.
    pub fn focused_thread(&self) -> Option<&str> {
        self.navigation
            .focused_card
            .as_ref()?
            .thread()
            .map(|id| &**id)
    }

    /// The focused draft, by the index the drafts are held at.
    pub fn focused_draft(&self) -> Option<usize> {
        self.draft_by_id(self.navigation.focused_card.as_ref()?.draft()?)
    }

    /// The row the cursor sits on: a focused thread's summary when one is
    /// focused, otherwise the source line itself.
    fn cursor_row(&self, rows: &Rows) -> usize {
        let focused = self
            .navigation
            .focused_card
            .as_ref()
            .and_then(|card| rows.card_row(card));

        focused.unwrap_or_else(|| rows.code_row(self.navigation.cursor))
    }

    /// Scrolls a landing into view, and gives a card the room its conversation
    /// needs below it.
    ///
    /// A card unfolds downwards, so one brought only just into view opens off
    /// the foot of the pane: the reader would have to scroll it before reading
    /// it, and while it is open the keys that scroll the pane are the ones it
    /// takes for itself. A card that cannot fit open where it stands is
    /// anchored near the top instead, with a little of the code it hangs under
    /// left above it.
    fn reveal_card(&mut self, rows: &Rows, viewport: usize) {
        if viewport == 0 {
            return;
        }

        let placed = self.navigation.focused_card.as_ref().and_then(|card| {
            Some((rows.card_row(card)?, rows.card_height(card)))
        });

        let Some((row, height)) = placed else {
            self.keep_in_view(rows, viewport);
            return;
        };

        let needed =
            height.max(rows::thread_window(viewport) + 1).min(viewport);

        if row >= self.navigation.diff_scroll
            && row + needed <= self.navigation.diff_scroll + viewport
        {
            return;
        }

        let context = 3.min(viewport / 4);
        self.navigation.diff_scroll = row
            .saturating_sub(context)
            .min(rows.len().saturating_sub(viewport));
    }

    /// Keeps the cursor inside the viewport with a small scroll-off margin.
    ///
    /// The row list is the one built before this keystroke, which is also the
    /// one the renderer will slice, so a motion and the scroll that follows it
    /// agree even though the list is a frame behind.
    fn follow_cursor(&mut self, layout: &Layout) {
        self.scroll_into_view(layout, layout.diff_viewport());
    }

    pub(super) fn scroll_into_view(
        &mut self,
        layout: &Layout,
        viewport: usize,
    ) {
        self.keep_in_view(&layout.rows, viewport);
    }

    /// Keeps the cursor inside `viewport` with a small scroll-off margin,
    /// moving no further than it has to.
    fn keep_in_view(&mut self, rows: &Rows, viewport: usize) {
        if viewport == 0 {
            return;
        }

        let row = self.cursor_row(rows);
        let margin = 3.min(viewport / 4);

        if row < self.navigation.diff_scroll + margin {
            self.navigation.diff_scroll = row.saturating_sub(margin);
            return;
        }

        let bottom =
            self.navigation.diff_scroll + viewport.saturating_sub(margin + 1);
        if row > bottom {
            self.navigation.diff_scroll =
                (row + margin + 1).saturating_sub(viewport);
        }
    }
}
