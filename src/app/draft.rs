use crate::model::{ChangedFile, LineKind};
use std::ops::RangeInclusive;

pub use crate::model::Side;

/// A review comment written but not yet submitted. Held locally so a whole
/// review can be composed offline and sent in one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub path: String,
    pub start_line: u32,
    pub end_line: u32,
    pub side: Side,
    pub body: String,
}

impl Draft {
    pub fn is_multiline(&self) -> bool {
        self.start_line != self.end_line
    }

    pub fn covers(&self, path: &str, line: u32, side: Side) -> bool {
        self.path == path && self.side == side && (self.start_line..=self.end_line).contains(&line)
    }
}

/// Where a selection of diff rows anchors in the file, once blank rows and
/// hunk headers are discarded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub start_line: u32,
    pub end_line: u32,
    pub side: Side,
}

/// A selection that is entirely removals comments on the old side; anything
/// else anchors to the new side, which is what GitHub expects for additions
/// and untouched context.
pub fn anchor_for(file: &ChangedFile, rows: RangeInclusive<usize>) -> Option<Anchor> {
    let selected: Vec<&crate::model::DiffLine> = file
        .lines
        .get(rows)
        .unwrap_or_default()
        .iter()
        .filter(|line| line.kind != LineKind::Hunk)
        .collect();

    if selected.is_empty() {
        return None;
    }

    let side = if selected.iter().all(|line| line.kind == LineKind::Removed) {
        Side::Left
    } else {
        Side::Right
    };

    let numbers: Vec<u32> = selected
        .iter()
        .filter_map(|line| match side {
            Side::Left => line.old_line,
            Side::Right => line.new_line,
        })
        .collect();

    Some(Anchor {
        start_line: *numbers.iter().min()?,
        end_line: *numbers.iter().max()?,
        side,
    })
}
