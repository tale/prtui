//! Compatibility re-exports for callers of the original highlighting module.

pub use crate::renderer::Segment;

use crate::model::DiffLine;

pub fn preload() {
    crate::renderer::Renderer::default().preload();
}

pub fn highlight_file(path: &str, lines: &[DiffLine]) -> Vec<Vec<Segment>> {
    crate::renderer::Renderer::default().highlight_file(path, lines)
}
