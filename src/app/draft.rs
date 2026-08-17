use crate::model::{ChangedFile, DiffLine, LineKind};
use std::ops::RangeInclusive;

pub use crate::model::Side;

/// Where a comment lands in the file, in GitHub's terms: a start and an end,
/// each on its own side of the diff.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    pub start_line: u32,
    pub start_side: Side,
    pub end_line: u32,
    pub side: Side,
}

impl Anchor {
    const fn spanning(start_line: u32, end_line: u32, side: Side) -> Self {
        Self {
            start_line,
            start_side: side,
            end_line,
            side,
        }
    }

    /// A span that crosses sides is multi-line even when the two line numbers
    /// happen to match, since they count in different files.
    pub fn is_multiline(&self) -> bool {
        self.start_line != self.end_line || self.start_side != self.side
    }
}

/// A review comment written but not yet submitted. Held locally so a whole
/// review can be composed offline and sent in one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub path: String,
    /// The diff rows the comment was written against. The anchor is what
    /// GitHub is told; this is what the gutter marks and the cursor tests.
    pub rows: RangeInclusive<usize>,
    pub anchor: Anchor,
    pub body: String,
}

impl Draft {
    pub fn covers(&self, path: &str, row: usize) -> bool {
        self.path == path && self.rows.contains(&row)
    }

    pub fn overlaps(&self, path: &str, rows: &RangeInclusive<usize>) -> bool {
        self.path == path
            && self.rows.start() <= rows.end()
            && rows.start() <= self.rows.end()
    }

    /// One entry of a review's `comments` array. GitHub anchors a span at its
    /// last line and takes the first as `start_line`, which is only sent when
    /// the comment actually covers more than one.
    pub fn to_api(&self) -> serde_json::Value {
        let mut comment = serde_json::json!({
            "path": self.path,
            "body": self.body,
            "line": self.anchor.end_line,
            "side": self.anchor.side.as_api(),
        });

        if self.anchor.is_multiline() {
            comment["start_line"] = self.anchor.start_line.into();
            comment["start_side"] = self.anchor.start_side.as_api().into();
        }

        comment
    }
}

/// Resolves a span of diff rows to the anchor GitHub understands.
///
/// Deletions count against the old file and everything else against the new
/// one, so a selection running from one into the other spans both sides —
/// which is exactly what `start_side` and `side` exist to express. Ending back
/// on a deletion has no such form, so that selection stays wholly on the left
/// rather than pairing a right-hand start with a left-hand end.
pub fn anchor_for(
    file: &ChangedFile,
    rows: RangeInclusive<usize>,
) -> Option<Anchor> {
    let selected: Vec<&DiffLine> = file
        .lines
        .get(rows)
        .unwrap_or_default()
        .iter()
        .filter(|line| line.kind != LineKind::Hunk)
        .collect();

    let is_removal = |line: &&&DiffLine| line.kind == LineKind::Removed;
    let deleted: Vec<u32> = selected
        .iter()
        .filter(is_removal)
        .filter_map(|line| line.old_line)
        .collect();
    let present: Vec<u32> = selected
        .iter()
        .filter(|line| !is_removal(line))
        .filter_map(|line| line.new_line)
        .collect();

    let ends_on_a_deletion = selected
        .last()
        .is_some_and(|line| line.kind == LineKind::Removed);

    if present.is_empty() || ends_on_a_deletion {
        return Some(Anchor::spanning(
            *deleted.iter().min()?,
            *deleted.iter().max()?,
            Side::Left,
        ));
    }

    if deleted.is_empty() {
        return Some(Anchor::spanning(
            *present.iter().min()?,
            *present.iter().max()?,
            Side::Right,
        ));
    }

    Some(Anchor {
        start_line: *deleted.iter().min()?,
        start_side: Side::Left,
        end_line: *present.iter().max()?,
        side: Side::Right,
    })
}
