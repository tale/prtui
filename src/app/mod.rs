pub mod action;
pub mod draft;
pub mod editor;
pub mod input;
pub mod keymap;
pub mod mode;

use crate::model::{ChangedFile, PullRequest, ReviewThread};
use crate::renderer::{Renderer, Segment, Theme, ThemeMode};
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

pub struct App {
    pub pr: Option<PullRequest>,
    pub files: Vec<ChangedFile>,
    pub threads_by_path: HashMap<String, Vec<ReviewThread>>,
    pub drafts: Vec<Draft>,

    pub mode: Mode,
    pub selection: Option<Selection>,
    pub composer: Option<Composer>,
    pub selected_file: usize,
    pub cursor: usize,
    pub diff_scroll: usize,
    pub pane: Pane,
    pub is_files_visible: bool,

    pub status: String,
    pub load_ms: Option<u128>,
    pub should_quit: bool,

    renderer: Renderer,
    highlights: HashMap<usize, Vec<Vec<Segment>>>,
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
            mode: Mode::Normal,
            selection: None,
            composer: None,
            selected_file: 0,
            cursor: 0,
            diff_scroll: 0,
            pane: Pane::Files,
            is_files_visible: true,
            status: "loading…".into(),
            load_ms: None,
            should_quit: false,
            renderer,
            highlights: HashMap::new(),
        }
    }

    pub const fn theme(&self) -> Theme {
        self.renderer.theme()
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

    pub fn drafts_for_current(&self) -> impl Iterator<Item = &Draft> {
        let path = self
            .current_file()
            .map(|f| f.path.clone())
            .unwrap_or_default();

        self.drafts.iter().filter(move |d| d.path == path)
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

    pub fn set_highlight(&mut self, index: usize, styled: Vec<Vec<Segment>>) {
        self.highlights.entry(index).or_insert(styled);
    }

    /// Bulk result from the background pass; never clobbers a file that was
    /// already highlighted on demand.
    pub fn set_highlights(&mut self, all: Vec<Vec<Vec<Segment>>>) {
        for (index, styled) in all.into_iter().enumerate() {
            self.set_highlight(index, styled);
        }
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
                if !self.is_files_visible {
                    self.pane = Pane::Diff;
                }
            }
            Action::NextFile => self.select_file(self.selected_file.saturating_add(1)),
            Action::PrevFile => self.select_file(self.selected_file.saturating_sub(1)),
            Action::Move(motion) => self.travel(motion, viewport_height),

            Action::EnterVisual => {
                if self.pane == Pane::Diff {
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

        self.status = format!("{} draft comment(s)", self.drafts.len());
    }

    fn toggle_pane(&mut self) {
        if !self.is_files_visible || self.mode == Mode::Visual {
            return;
        }

        self.pane = if self.pane == Pane::Files {
            Pane::Diff
        } else {
            Pane::Files
        };
    }

    fn select_file(&mut self, index: usize) {
        if self.files.is_empty() {
            return;
        }

        self.selected_file = index.min(self.files.len() - 1);
        self.cursor = 0;
        self.diff_scroll = 0;
        self.selection = None;
        self.mode = Mode::Normal;
    }

    fn travel(&mut self, motion: Motion, viewport_height: usize) {
        if self.pane == Pane::Files {
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
        self.cursor = match motion {
            Motion::Down(n) => self.cursor.saturating_add(n).min(last),
            Motion::Up(n) => self.cursor.saturating_sub(n),
            Motion::HalfPageDown => self.cursor.saturating_add(viewport_height / 2).min(last),
            Motion::HalfPageUp => self.cursor.saturating_sub(viewport_height / 2),
            Motion::Top => 0,
            Motion::Bottom => last,
        };

        if let Some(selection) = &mut self.selection {
            selection.head = self.cursor;
        }

        self.follow_cursor(viewport_height);
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
