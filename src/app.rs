use crate::highlight::{self, Segment};
use crate::model::{ChangedFile, PullRequest, ReviewThread};
use std::collections::HashMap;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Pane {
    Files,
    Diff,
}

pub struct App {
    pub pr: Option<PullRequest>,
    pub files: Vec<ChangedFile>,
    pub threads_by_path: HashMap<String, Vec<ReviewThread>>,
    pub selected_file: usize,
    pub cursor: usize,
    pub diff_scroll: usize,
    pub pane: Pane,
    pub status: String,
    pub load_ms: Option<u128>,
    pub should_quit: bool,
    pub is_files_visible: bool,
    highlights: HashMap<usize, Vec<Vec<Segment>>>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        Self {
            pr: None,
            files: Vec::new(),
            threads_by_path: HashMap::new(),
            selected_file: 0,
            cursor: 0,
            diff_scroll: 0,
            pane: Pane::Files,
            status: "loading…".into(),
            load_ms: None,
            should_quit: false,
            is_files_visible: true,
            highlights: HashMap::new(),
        }
    }

    pub fn set_meta(&mut self, pr: PullRequest) {
        let mut by_path: HashMap<String, Vec<ReviewThread>> = HashMap::new();
        for thread in &pr.threads {
            by_path.entry(thread.path.clone()).or_default().push(thread.clone());
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

    /// Highlighting is deferred until a file is actually shown, then kept.
    pub fn ensure_highlighted(&mut self) {
        let index = self.selected_file;
        if self.highlights.contains_key(&index) {
            return;
        }

        let Some(file) = self.files.get(index) else { return };

        let styled = highlight::highlight_file(&file.path, &file.lines);
        self.highlights.insert(index, styled);
    }

    /// Bulk result from the background pass; never clobbers a file that was
    /// already highlighted on demand.
    pub fn set_highlights(&mut self, all: Vec<Vec<Vec<Segment>>>) {
        for (index, styled) in all.into_iter().enumerate() {
            self.highlights.entry(index).or_insert(styled);
        }
    }

    pub fn highlighted(&self) -> Option<&[Vec<Segment>]> {
        self.highlights.get(&self.selected_file).map(|v| v.as_slice())
    }

    fn select_file(&mut self, index: usize) {
        if self.files.is_empty() {
            return;
        }

        self.selected_file = index.min(self.files.len() - 1);
        self.cursor = 0;
        self.diff_scroll = 0;
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

    pub fn on_key(&mut self, key: char, viewport_height: usize) {
        match key {
            'q' => self.should_quit = true,
            '\t' => self.toggle_pane(),
            'f' => {
                self.is_files_visible = !self.is_files_visible;
                if !self.is_files_visible {
                    self.pane = Pane::Diff;
                }
            }
            'j' => self.move_down(1, viewport_height),
            'k' => self.move_up(1, viewport_height),
            'd' => self.move_down(viewport_height / 2, viewport_height),
            'u' => self.move_up(viewport_height / 2, viewport_height),
            'g' => match self.pane {
                Pane::Files => self.select_file(0),
                Pane::Diff => {
                    self.cursor = 0;
                    self.follow_cursor(viewport_height);
                }
            },
            'G' => match self.pane {
                Pane::Files => self.select_file(self.files.len().saturating_sub(1)),
                Pane::Diff => {
                    self.cursor = self.diff_len().saturating_sub(1);
                    self.follow_cursor(viewport_height);
                }
            },
            ']' => self.select_file(self.selected_file + 1),
            '[' => self.select_file(self.selected_file.saturating_sub(1)),
            _ => {}
        }
    }

    fn toggle_pane(&mut self) {
        if !self.is_files_visible {
            return;
        }

        self.pane = if self.pane == Pane::Files { Pane::Diff } else { Pane::Files };
    }

    fn move_down(&mut self, amount: usize, viewport_height: usize) {
        if self.pane == Pane::Files {
            self.select_file(self.selected_file + amount);
            return;
        }

        let last = self.diff_len().saturating_sub(1);
        self.cursor = (self.cursor + amount).min(last);
        self.follow_cursor(viewport_height);
    }

    fn move_up(&mut self, amount: usize, viewport_height: usize) {
        if self.pane == Pane::Files {
            self.select_file(self.selected_file.saturating_sub(amount));
            return;
        }

        self.cursor = self.cursor.saturating_sub(amount);
        self.follow_cursor(viewport_height);
    }
}
