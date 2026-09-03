//! One coalescing syntax worker for the review session.
//!
//! Highlighting is CPU-bound and cannot be cancelled once syntect is inside a
//! line. Keeping one worker bounds that cost; generations make the result of
//! the one task already running harmless when its file or palette changes.

use crate::app::Highlight;
use crate::renderer::{self, ThemeMode};
use prtui_core::ChangedFile;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};

pub struct Output {
    pub generation: u64,
    pub mode: ThemeMode,
    pub path: Arc<str>,
    pub styled: Highlight,
}

struct Task {
    generation: u64,
    mode: ThemeMode,
    file: Arc<ChangedFile>,
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
struct Queue {
    tasks: VecDeque<Task>,
    is_closed: bool,
}

#[derive(Default)]
struct Pending {
    queue: Mutex<Queue>,
    ready: Condvar,
}

impl Pending {
    fn replace(&self, tasks: impl IntoIterator<Item = Task>) {
        let mut pending = lock(&self.queue);
        pending.tasks.clear();
        pending.tasks.extend(tasks);
        drop(pending);
        self.ready.notify_one();
    }

    fn prioritize(&self, task: Task) {
        let mut pending = lock(&self.queue);
        pending
            .tasks
            .retain(|held| held.file.path != task.file.path);
        pending.tasks.push_front(task);
        drop(pending);
        self.ready.notify_one();
    }

    fn take(&self) -> Option<Task> {
        let mut pending = lock(&self.queue);
        loop {
            if let Some(task) = pending.tasks.pop_front() {
                return Some(task);
            }
            if pending.is_closed {
                return None;
            }
            pending = self
                .ready
                .wait(pending)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }

    fn close(&self) {
        let mut pending = lock(&self.queue);
        pending.tasks.clear();
        pending.is_closed = true;
        drop(pending);
        self.ready.notify_one();
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
            while let Some(task) = worker_pending.take() {
                if !lock(&worker_generations)
                    .is_current(&task.file.path, task.generation)
                {
                    continue;
                }

                let styled = renderer::highlight_file(
                    &task.file.path,
                    &task.file.lines,
                    task.mode,
                );
                if !lock(&worker_generations)
                    .is_current(&task.file.path, task.generation)
                {
                    continue;
                }

                publish(Output {
                    generation: task.generation,
                    mode: task.mode,
                    path: task.file.path.clone(),
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
        files: &[Arc<ChangedFile>],
        first: usize,
        mode: ThemeMode,
    ) {
        let order = highlight_order(files.len(), first);
        let mut generations = lock(&self.generations);
        generations.retire_all();
        let tasks: Vec<Task> = order
            .map(|index| {
                let file = files[index].clone();
                Task {
                    generation: generations.advance(file.path.clone()),
                    mode,
                    file,
                }
            })
            .collect();
        drop(generations);

        self.pending.replace(tasks);
    }

    /// Recolors a file whose patch changed, superseding any older queued pass
    /// for the same path without allowing work to grow without bound.
    pub fn one(&self, file: &Arc<ChangedFile>, mode: ThemeMode) {
        let generation = lock(&self.generations).advance(file.path.clone());

        self.pending.prioritize(Task {
            generation,
            mode,
            file: file.clone(),
        });
    }

    pub fn accepts(&self, output: &Output) -> bool {
        lock(&self.generations).is_current(&output.path, output.generation)
    }
}

impl Drop for Highlighter {
    fn drop(&mut self) {
        self.pending.close();
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
