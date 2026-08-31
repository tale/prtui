//! Command-line, search, and file-filter interaction.

use std::borrow::Cow;

use super::{
    App, Card, CommentEditor, Edit, FileFilterSnapshot, Mode, Pane, Query,
    ReviewThread, SearchOrigin, ex, search,
};
use crate::layout::Layout;
use crate::vim::step_hit;
use termina::event::KeyEvent;

impl App {
    /// `/` means "find what I am reading". Which surface that is comes from
    /// where the reader is: an open panel, then the tree, then the diff.
    pub(super) fn start_find(&mut self) {
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
    pub(super) fn escape(&mut self) {
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

    pub(super) fn start_command_line(&mut self) {
        self.command_line = Some(CommentEditor::default());
        self.history_cursor = None;
        self.status.clear();
        self.mode = Mode::CommandLine;
    }

    pub(super) fn cancel_command_line(&mut self) {
        self.command_line = None;
        self.history_cursor = None;
        self.mode = Mode::Normal;
    }

    /// Runs the line and hands the action straight back to `apply`, so a `:`
    /// command and the key bound to the same name take the same path.
    pub(super) fn run_command_line(&mut self, layout: &Layout) {
        let Some(editor) = self.command_line.take() else {
            self.mode = Mode::Normal;
            return;
        };

        self.history_cursor = None;
        self.mode = Mode::Normal;
        let line = editor.text();
        let action = ex::parse(&line);
        if self.command_history.last() != Some(&line) && !line.trim().is_empty()
        {
            self.command_history.push(line);
        }

        match action {
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
    pub(super) fn walk_history(&mut self, direction: isize, layout: &Layout) {
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

    /// Runs a readline edit against whichever line the mode is editing.
    pub(super) fn edit_line(&mut self, edit: Edit, layout: &Layout) {
        // A motion inside a recalled line is not a move off it, so only an
        // edit that changes the text gives up the place in the history.
        let recalled = self.history_cursor;

        self.edit_prompt(layout, |editor| {
            editor.edit(edit);
        });

        if !edit.is_destructive() {
            self.history_cursor = recalled;
        }
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
        let body = if self.mode == Mode::Insert || !text.contains(['\r', '\n'])
        {
            Cow::Borrowed(text)
        } else {
            Cow::Owned(text.replace(['\r', '\n'], ""))
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
    pub(super) fn clear_find(&mut self) {
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

    pub(super) fn accept_file_filter(&mut self) {
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

    pub(super) fn cancel_file_filter(&mut self) {
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

    pub(super) fn accept_search(&mut self, layout: &Layout) {
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
        let is_found = if self.mode.is_overlay() {
            !self.overlay_matches(layout).is_empty()
        } else {
            !self.search_matches(layout).is_empty()
        };

        if !is_found {
            self.status = format!("pattern not found: {query}");
        }
        if self.search_history.last() != Some(&query) {
            self.search_history.push(query);
        }
    }

    pub(super) fn cancel_search(&mut self) {
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
        let stops = layout.rows.stops();
        let mut hits = stops
            .iter()
            .filter(|stop| self.card_matches(&stop.card, threads, query))
            .peekable();

        let mut matches = Vec::with_capacity(file.lines.len() + stops.len());
        for (row, line) in file.lines.iter().enumerate() {
            if query.is_match(&line.text) {
                matches.push(search::Match::Line(row));
            }

            while let Some(stop) = hits.next_if(|stop| stop.source == row) {
                matches.push(search::Match::Card {
                    row,
                    card: stop.card.clone(),
                });
            }
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
        matches.iter().position(|hit| match hit {
            search::Match::Line(row) => {
                *row == self.cursor && self.focused_card.is_none()
            }
            search::Match::Card { row, card } => {
                *row == self.cursor && Some(card) == self.focused_card.as_ref()
            }
        })
    }

    pub(super) fn jump_match(
        &mut self,
        direction: isize,
        count: usize,
        layout: &Layout,
    ) {
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

        if count == 0 {
            return;
        }

        // Searching is file-local, so both ends wrap rather than spilling
        // into the next file the way comment jumps do. From between hits, the
        // nearest hit consumes the first step; `step_hit` handles the rest.
        let current = self.match_position(&matches);
        let nearest = || {
            if direction > 0 {
                matches
                    .iter()
                    .position(|hit| hit.row() >= self.cursor)
                    .unwrap_or(0)
            } else {
                matches
                    .iter()
                    .rposition(|hit| hit.row() <= self.cursor)
                    .unwrap_or(matches.len() - 1)
            }
        };
        let start = current.unwrap_or_else(nearest);
        let remaining = count.saturating_sub(usize::from(current.is_none()));
        let Some(target) =
            step_hit(Some(start), matches.len(), direction, remaining)
        else {
            return;
        };

        let hit = &matches[target];
        self.land_on(hit.row(), hit.card(), layout);
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
}
