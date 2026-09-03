use super::draft::Draft;
use super::editor::CommentEditor;
use super::keymap::Keymap;
use super::mode::{Mode, Selection};
use super::review::Submission;
use super::{App, Card, Composer, Focus, OpenFile, Pane, TreeRow};
use crate::expand::Gap;
use crate::layout::Layout;
use crate::renderer::Theme;
use prtui_core::{ChangedFile, Comment, PullRequest, ReviewThread};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

/// Immutable application state consumed by layout and rendering.
///
/// Every collection is borrowed from [`App`]; creating this value allocates
/// and clones nothing.
#[derive(Clone, Copy)]
pub struct View<'a> {
    app: &'a App,
    pub pr: Option<&'a PullRequest>,
    pub files: &'a [Arc<ChangedFile>],
    pub threads_by_path: &'a HashMap<Arc<str>, Vec<ReviewThread>>,
    pub drafts: &'a [Draft],
    pub mode: Mode,
    pub selection: Option<Selection>,
    pub composer: Option<&'a Composer>,
    pub submission: Option<&'a Submission>,
    pub file_filter: Option<&'a CommentEditor>,
    pub search: Option<&'a CommentEditor>,
    pub command_line: Option<&'a CommentEditor>,
    pub selected_file: usize,
    pub cursor: usize,
    pub focused_card: Option<&'a Card>,
    pub expanded_card: Option<&'a Card>,
    pub thread_scroll: usize,
    pub diff_scroll: usize,
    pub pane: Pane,
    pub is_files_visible: bool,
    pub discussion: &'a [Comment],
    pub status: &'a str,
    pub loading_frame: usize,
    pub in_flight: usize,
    pub overlay_scroll: usize,
}

impl<'a> View<'a> {
    pub(super) fn new(app: &'a App) -> Self {
        Self {
            app,
            pr: app.pr.as_ref(),
            files: &app.files,
            threads_by_path: &app.threads_by_path,
            drafts: &app.drafts,
            mode: app.mode,
            selection: app.selection,
            composer: app.composer.as_ref(),
            submission: app.submission.as_ref(),
            file_filter: app.file_filter.as_ref(),
            search: app.search.as_ref(),
            command_line: app.command_line.as_ref(),
            selected_file: app.selected_file,
            cursor: app.cursor,
            focused_card: app.focused_card.as_ref(),
            expanded_card: app.expanded_card.as_ref(),
            thread_scroll: app.thread_scroll,
            diff_scroll: app.diff_scroll,
            pane: app.pane,
            is_files_visible: app.is_files_visible,
            discussion: &app.discussion,
            status: &app.status,
            loading_frame: app.loading_frame,
            in_flight: app.in_flight,
            overlay_scroll: app.overlay_scroll,
        }
    }

    pub const fn theme(self) -> Theme {
        self.app.theme()
    }

    pub const fn keymap(self) -> &'a Keymap {
        self.app.keymap()
    }

    pub fn pending_hint(self) -> String {
        self.app.pending_hint()
    }

    pub const fn is_loading(self) -> bool {
        self.app.is_loading()
    }

    pub fn is_status_alarming(self) -> bool {
        self.app.is_status_alarming()
    }

    pub fn current_file(self) -> Option<&'a ChangedFile> {
        self.app.current_file()
    }

    pub const fn collapsed(self) -> &'a HashSet<Arc<str>> {
        self.app.collapsed()
    }

    pub fn tree_directory(self) -> Option<&'a str> {
        self.app.tree_directory()
    }

    pub fn tree_query(self) -> Option<super::search::Query<'a>> {
        self.app.tree_query()
    }

    pub const fn files_placeholder(self) -> &'static str {
        self.app.files_placeholder()
    }

    pub fn unresolved_threads(self, path: &str) -> usize {
        self.app.unresolved_threads(path)
    }

    pub fn tree_row(self, index: usize) -> Option<TreeRow<'a>> {
        self.app.tree_row(index)
    }

    pub fn focus(self) -> Focus<'a> {
        self.app.focus()
    }

    pub fn open(self) -> Option<OpenFile<'a>> {
        self.app.open()
    }

    pub fn gaps(self) -> Vec<Gap> {
        self.app.gaps()
    }

    pub fn live_query(self) -> Option<super::search::Query<'a>> {
        self.app.live_query()
    }

    pub fn overlay_mode(self) -> Option<Mode> {
        self.app.overlay_mode()
    }

    pub fn is_searching_overlay(self) -> bool {
        self.app.is_searching_overlay()
    }

    pub fn overlay_match_row(self, layout: &Layout) -> Option<usize> {
        self.app.overlay_match_row(layout)
    }

    pub fn search_summary(self, layout: &Layout) -> (usize, usize) {
        self.app.search_summary(layout)
    }

    pub fn filtered_file_indices(self) -> Vec<usize> {
        self.app.filtered_file_indices()
    }

    pub fn focused_draft(self) -> Option<usize> {
        self.app.focused_draft()
    }
}
