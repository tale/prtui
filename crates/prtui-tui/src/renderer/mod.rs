mod highlight;
pub mod markdown;
mod theme;

pub use highlight::{Segment, highlight_file, preload};
pub use theme::{Theme, ThemeMode};
