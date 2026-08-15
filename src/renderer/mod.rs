mod highlight;
pub mod markdown;
mod theme;

pub use highlight::Segment;
pub use theme::{Theme, ThemeMode};

use crate::model::DiffLine;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

/// Immutable rendering configuration, cheap to copy into worker tasks.
#[derive(Debug, Clone, Copy)]
pub struct Renderer {
    theme: Theme,
}

impl Renderer {
    pub const fn new(mode: ThemeMode) -> Self {
        Self::with_theme(Theme::for_mode(mode))
    }

    pub const fn with_theme(theme: Theme) -> Self {
        Self { theme }
    }

    pub const fn theme(self) -> Theme {
        self.theme
    }

    pub fn highlight_file(
        self,
        path: &str,
        lines: &[DiffLine],
    ) -> Vec<Vec<Segment>> {
        highlight::highlight_file(path, lines, self.theme.mode)
    }

    /// Highlight independent files across one worker per core, publishing each
    /// result immediately instead of waiting for the slowest file. Workers claim
    /// the next index themselves, so a handful of enormous files cannot strand
    /// the rest behind a fixed split.
    pub fn highlight_files_parallel<F>(
        self,
        files: &[(String, Vec<DiffLine>)],
        publish: F,
    ) where
        F: Fn(usize, Vec<Vec<Segment>>) + Sync + Send,
    {
        let workers = thread::available_parallelism()
            .map_or(1, NonZeroUsize::get)
            .min(files.len());
        let next = AtomicUsize::new(0);

        thread::scope(|scope| {
            for _ in 0..workers {
                scope.spawn(|| {
                    loop {
                        let index = next.fetch_add(1, Ordering::Relaxed);
                        let Some((path, lines)) = files.get(index) else {
                            break;
                        };

                        publish(index, self.highlight_file(path, lines));
                    }
                });
            }
        });
    }

    /// Deserialize the expensive syntax database before files arrive.
    pub fn preload(self) {
        highlight::preload(self.theme.mode);
    }
}

impl Default for Renderer {
    fn default() -> Self {
        Self::new(ThemeMode::Dark)
    }
}
