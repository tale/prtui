mod highlight;
pub mod markdown;
mod scheme;
mod theme;

pub use highlight::{Segment, highlight_file, preload};
pub use scheme::Scheme;
pub use theme::{Theme, ThemeMode};
