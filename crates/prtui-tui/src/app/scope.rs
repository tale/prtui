//! Reading some of a pull request's commits in place of all of them.

use super::effect::Effect;
use super::mode::Mode;
use super::{App, ReviewState};
use crate::commits::{self, Entry, Pick};
use crate::layout::{Content, Layout, panel_area, panel_inner};
use crate::vim::Cursor;
use prtui_core::{ChangedFile, CommitLog, ReviewThread};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

#[derive(Default)]
pub enum CommitsState {
    #[default]
    Absent,
    Loading,
    Ready(Box<CommitLog>),
    Failed(String),
}

/// The files of a pick other than [`Pick::All`], shown in place of the pull
/// request's own. Threads and drafts stay filed against those.
pub(super) struct Scope {
    pub head: Arc<str>,
    pub label: String,
    pub files: Vec<Arc<ChangedFile>>,
}

/// A pick whose files are on the way.
pub(super) struct Requested {
    generation: u64,
    pick: Pick,
    range: Range,
}

/// The two ends of the diff a pick names. `label` is short enough for the
/// header; `description` is what the status line says was loaded.
struct Range {
    base: Arc<str>,
    head: Arc<str>,
    label: String,
    description: String,
}

impl ReviewState {
    pub(super) const fn shown_files_mut(
        &mut self,
    ) -> &mut Vec<Arc<ChangedFile>> {
        match &mut self.scope {
            Some(scope) => &mut scope.files,
            None => &mut self.files,
        }
    }
}

static NO_THREADS: LazyLock<HashMap<Arc<str>, Vec<ReviewThread>>> =
    LazyLock::new(HashMap::new);

impl App {
    pub(super) fn shown_files(&self) -> &[Arc<ChangedFile>] {
        self.review
            .scope
            .as_ref()
            .map_or(&self.review.files, |scope| &scope.files)
    }

    /// A thread's lines are numbered against the whole pull request, so none
    /// of them can be placed on a diff of part of it.
    pub(super) fn shown_threads(
        &self,
    ) -> &HashMap<Arc<str>, Vec<ReviewThread>> {
        if self.review.scope.is_some() {
            return &NO_THREADS;
        }

        &self.review.threads_by_path
    }

    pub fn scope_label(&self) -> Option<&str> {
        self.review.scope.as_ref().map(|scope| scope.label.as_str())
    }

    /// The commit the shown files are read at.
    pub(super) fn head_commit(&self) -> Option<Arc<str>> {
        if let Some(scope) = &self.review.scope {
            return Some(scope.head.clone());
        }

        self.review.pr.as_ref().map(|pr| pr.head_oid.clone())
    }

    pub(super) fn open_commits(&mut self, layout: &Layout) {
        self.navigation.mode = Mode::Commits;
        self.navigation.overlay = Cursor::default();
        self.navigation.overlay_match = None;
        self.navigation.commit_anchor = None;

        if let CommitsState::Ready(log) = &self.review.commits {
            self.navigation.overlay =
                current_row(log, &self.review.pick, layout);
        }

        self.request_commits();
    }

    pub(super) fn request_commits(&mut self) {
        self.review.commits_generation =
            self.review.commits_generation.wrapping_add(1);
        let generation = self.review.commits_generation;

        // A log already in hand stays on screen while its refresh is out.
        if !matches!(self.review.commits, CommitsState::Ready(_)) {
            self.review.commits = CommitsState::Loading;
        }
        self.runtime
            .effects
            .push(Effect::FetchCommits { generation });
    }

    pub(super) fn receive_commits(
        &mut self,
        generation: u64,
        outcome: Result<Box<CommitLog>, String>,
    ) -> bool {
        if generation != self.review.commits_generation {
            return false;
        }

        self.review.commits = match outcome {
            Ok(log) => CommitsState::Ready(log),
            Err(error) => CommitsState::Failed(error),
        };
        self.navigation.commit_anchor = None;
        true
    }

    pub(super) fn receive_range(
        &mut self,
        generation: u64,
        outcome: Result<Vec<ChangedFile>, String>,
    ) -> bool {
        let Some(requested) = self
            .review
            .requested
            .take_if(|requested| requested.generation == generation)
        else {
            return false;
        };

        let range = requested.range;
        let files = match outcome {
            Ok(files) => files,
            Err(error) => {
                self.runtime.status = format!("error: {error}");
                return true;
            }
        };

        if files.is_empty() {
            self.runtime.status =
                format!("no changes in {}", range.description);
            return true;
        }

        self.runtime.status = format!("showing {}", range.description);
        self.review.pick = requested.pick;
        self.show(Some(Scope {
            head: range.head,
            label: range.label,
            files: files.into_iter().map(Arc::new).collect(),
        }));
        true
    }

    /// The commit rows between where `v` was pressed and the cursor.
    pub fn commit_span(&self) -> Option<(usize, usize)> {
        let anchor = self.navigation.commit_anchor?;
        let CommitsState::Ready(log) = &self.review.commits else {
            return None;
        };
        let cursor = self
            .navigation
            .overlay
            .index
            .checked_sub(commits::first_commit_row(log))?
            .min(log.commits.len().checked_sub(1)?);

        Some((anchor.min(cursor), anchor.max(cursor)))
    }

    pub(super) fn toggle_commit_span(&mut self, layout: &Layout) {
        if self.navigation.commit_anchor.take().is_some() {
            return;
        }

        let Some(Entry::Commit(index)) = self.entry_at_cursor(layout) else {
            self.runtime.status = "a range starts on a commit".into();
            return;
        };

        self.navigation.commit_anchor = Some(index);
    }

    pub(super) fn activate_commit(&mut self, layout: &Layout) {
        let CommitsState::Ready(log) = &self.review.commits else {
            return;
        };

        let pick = if let Some((first, last)) = self.commit_span() {
            Pick::Commits {
                first: log.commits[first].oid.clone(),
                last: log.commits[last].oid.clone(),
            }
        } else {
            match self.entry_at_cursor(layout) {
                Some(Entry::All) => Pick::All,
                Some(Entry::SinceReview) => Pick::SinceReview,
                Some(Entry::Commit(index)) => {
                    let oid = log.commits[index].oid.clone();
                    Pick::Commits {
                        first: oid.clone(),
                        last: oid,
                    }
                }
                None => return,
            }
        };

        self.choose(pick);
    }

    fn entry_at_cursor(&self, layout: &Layout) -> Option<Entry> {
        let Some(Content::Commits(rows)) =
            layout.overlay.as_ref().map(|overlay| &overlay.content)
        else {
            return None;
        };

        rows.entry_at(self.navigation.overlay.index)
    }

    fn choose(&mut self, pick: Pick) {
        let CommitsState::Ready(log) = &self.review.commits else {
            return;
        };

        if pick == self.review.pick {
            self.close_commits();
            return;
        }

        if pick == Pick::All {
            self.review.requested = None;
            self.review.pick = Pick::All;
            self.show(None);
            self.close_commits();
            self.runtime.status = "showing all changes".into();
            return;
        }

        let range = match resolve(log, &pick, self.review.pr.as_ref()) {
            Ok(range) => range,
            Err(message) => {
                self.runtime.status = message.into();
                return;
            }
        };

        self.review.range_generation =
            self.review.range_generation.wrapping_add(1);
        let generation = self.review.range_generation;

        self.runtime.status = format!("loading {}…", range.description);
        self.runtime.effects.push(Effect::FetchRange {
            generation,
            base: range.base.clone(),
            head: range.head.clone(),
        });
        self.review.requested = Some(Requested {
            generation,
            pick,
            range,
        });
        self.close_commits();
    }

    fn close_commits(&mut self) {
        self.navigation.mode = Mode::Normal;
        self.navigation.overlay = Cursor::default();
        self.navigation.overlay_match = None;
        self.navigation.commit_anchor = None;
    }

    /// Swaps the file set under the reader, staying on the open file when the
    /// new set has it. Colors and contents were taken at another commit.
    fn show(&mut self, scope: Option<Scope>) {
        let path = self.current_path().map(Arc::<str>::from);

        self.review.scope = scope;
        self.highlights.clear();
        self.blobs.clear();
        self.fetching.clear();
        self.deferred = None;
        self.navigation.expanded_card = None;

        let index = path
            .and_then(|path| {
                self.shown_files().iter().position(|file| file.path == path)
            })
            .unwrap_or(0);
        self.set_selected_file(index, true);

        self.runtime.effects.push(Effect::HighlightAll);
    }
}

/// Where the cursor opens: on whatever the diff is showing now.
fn current_row(log: &CommitLog, pick: &Pick, layout: &Layout) -> Cursor {
    let Some(entry) = commits::entry_of(log, pick) else {
        return Cursor::default();
    };
    let row = match entry {
        Entry::All => 0,
        Entry::SinceReview => 1,
        Entry::Commit(index) => commits::first_commit_row(log) + index,
    };
    let viewport = panel_inner(panel_area(layout.body)).height as usize;
    let len = commits::first_commit_row(log) + log.commits.len();

    let mut cursor = Cursor::default();
    cursor.jump(row, len, viewport);
    cursor
}

fn resolve(
    log: &CommitLog,
    pick: &Pick,
    pr: Option<&prtui_core::PullRequest>,
) -> Result<Range, &'static str> {
    match pick {
        Pick::All => Err("nothing to load"),
        Pick::SinceReview => {
            let base = log
                .last_reviewed
                .clone()
                .ok_or("no review of yours to diff from")?;
            let head = pr
                .map(|pr| pr.head_oid.clone())
                .ok_or("still loading the pull request")?;
            if base == head {
                return Err("nothing new since your last review");
            }

            Ok(Range {
                base,
                head,
                label: "since review".into(),
                description: "changes since your last review".into(),
            })
        }
        Pick::Commits { first, last } => {
            let find = |oid: &Arc<str>| {
                log.commits.iter().position(|commit| commit.oid == *oid)
            };
            let (Some(from), Some(to)) = (find(first), find(last)) else {
                return Err("that commit is no longer on the branch");
            };
            let base = log.commits[from]
                .parent
                .clone()
                .ok_or("that commit has no parent to diff against")?;

            let (label, description) = if from == to {
                let label = commits::short(first).to_owned();
                let description =
                    format!("{label} {}", log.commits[from].title);
                (label, description)
            } else {
                let label = format!(
                    "{}..{}",
                    commits::short(first),
                    commits::short(last)
                );
                let description = format!("{} commits", to - from + 1);
                (label, description)
            };

            Ok(Range {
                base,
                head: last.clone(),
                label,
                description,
            })
        }
    }
}
