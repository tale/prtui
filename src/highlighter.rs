//! One coalescing syntax worker for the review session.
//!
//! Highlighting is CPU-bound and cannot be cancelled once syntect is inside a
//! line. Keeping one worker bounds that cost; generations make the result of
//! the one task already running harmless when its file or palette changes.

use prtui::app::Highlight;
use prtui::model::{ChangedFile, DiffLine};
use prtui::renderer::{self, ThemeMode};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

pub struct Output {
    pub generation: u64,
    pub mode: ThemeMode,
    pub path: Arc<str>,
    pub styled: Highlight,
}

enum Input {
    Shared {
        files: Arc<[ChangedFile]>,
        index: usize,
    },
    Owned(Vec<DiffLine>),
}

impl Input {
    fn lines(&self) -> &[DiffLine] {
        match self {
            Self::Shared { files, index } => &files[*index].lines,
            Self::Owned(lines) => lines,
        }
    }
}

struct Task {
    generation: u64,
    mode: ThemeMode,
    path: Arc<str>,
    input: Input,
}

#[derive(Default)]
struct Generations {
    next: u64,
    current: HashMap<Arc<str>, u64>,
}

impl Generations {
    fn retire_all(&mut self) {
        self.current.clear();
    }

    fn advance(&mut self, path: Arc<str>) -> u64 {
        self.next = self.next.wrapping_add(1);
        self.current.insert(path, self.next);
        self.next
    }

    fn is_current(&self, path: &str, generation: u64) -> bool {
        self.current.get(path) == Some(&generation)
    }
}

#[derive(Default)]
struct Pending {
    tasks: Mutex<VecDeque<Task>>,
    ready: Condvar,
}

impl Pending {
    fn replace(&self, tasks: impl IntoIterator<Item = Task>) {
        let mut pending = lock(&self.tasks);
        pending.clear();
        pending.extend(tasks);
        drop(pending);
        self.ready.notify_one();
    }

    fn prioritize(&self, task: Task) {
        let mut pending = lock(&self.tasks);
        pending.retain(|held| held.path != task.path);
        pending.push_front(task);
        drop(pending);
        self.ready.notify_one();
    }

    fn take(&self) -> Task {
        let mut pending = lock(&self.tasks);
        loop {
            if let Some(task) = pending.pop_front() {
                return task;
            }
            pending = self
                .ready
                .wait(pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

pub struct Highlighter {
    generations: Arc<Mutex<Generations>>,
    pending: Arc<Pending>,
}

impl Highlighter {
    pub fn new(publish: impl Fn(Output) + Send + 'static) -> Self {
        let generations = Arc::new(Mutex::new(Generations::default()));
        let pending = Arc::new(Pending::default());
        let worker_generations = generations.clone();
        let worker_pending = pending.clone();

        std::thread::spawn(move || {
            loop {
                let task = worker_pending.take();
                if !lock(&worker_generations)
                    .is_current(&task.path, task.generation)
                {
                    continue;
                }

                let styled = renderer::highlight_file(
                    &task.path,
                    task.input.lines(),
                    task.mode,
                );
                if !lock(&worker_generations)
                    .is_current(&task.path, task.generation)
                {
                    continue;
                }

                publish(Output {
                    generation: task.generation,
                    mode: task.mode,
                    path: task.path,
                    styled,
                });
            }
        });

        Self {
            generations,
            pending,
        }
    }

    /// Replaces every queued pass. The file on screen leads so its first paint
    /// does not wait behind work the reader cannot see yet.
    pub fn all(
        &self,
        files: &Arc<[ChangedFile]>,
        first: usize,
        mode: ThemeMode,
    ) {
        let order = highlight_order(files.len(), first);
        let mut generations = lock(&self.generations);
        generations.retire_all();
        let tasks: Vec<Task> = order
            .map(|index| {
                let path = files[index].path.clone();
                Task {
                    generation: generations.advance(path.clone()),
                    mode,
                    path,
                    input: Input::Shared {
                        files: files.clone(),
                        index,
                    },
                }
            })
            .collect();
        drop(generations);

        self.pending.replace(tasks);
    }

    /// Recolors a file whose patch changed, superseding any older queued pass
    /// for the same path without allowing work to grow without bound.
    pub fn one(&self, file: &ChangedFile, mode: ThemeMode) {
        let path = file.path.clone();
        let generation = lock(&self.generations).advance(path.clone());

        self.pending.prioritize(Task {
            generation,
            mode,
            path,
            input: Input::Owned(file.lines.clone()),
        });
    }

    pub fn accepts(&self, output: &Output) -> bool {
        lock(&self.generations).is_current(&output.path, output.generation)
    }
}

/// The file being read first, then everything else in order.
fn highlight_order(count: usize, first: usize) -> impl Iterator<Item = usize> {
    std::iter::once(first)
        .filter(move |index| *index < count)
        .chain((0..count).filter(move |index| *index != first))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_open_file_is_colored_before_the_rest() {
        assert_eq!(highlight_order(4, 2).collect::<Vec<_>>(), [2, 0, 1, 3]);
        assert_eq!(highlight_order(3, 0).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(highlight_order(0, 0).next().is_none());
    }

    #[test]
    fn only_the_latest_generation_for_a_path_is_current() {
        let path: Arc<str> = "src/main.rs".into();
        let mut generations = Generations::default();
        let old = generations.advance(path.clone());
        let new = generations.advance(path.clone());

        assert!(!generations.is_current(&path, old));
        assert!(generations.is_current(&path, new));
    }

    #[test]
    fn a_new_batch_retires_paths_it_no_longer_contains() {
        let old: Arc<str> = "old.rs".into();
        let new: Arc<str> = "new.rs".into();
        let mut generations = Generations::default();
        let old_generation = generations.advance(old.clone());

        generations.retire_all();
        generations.advance(new);

        assert!(!generations.is_current(&old, old_generation));
    }
}
