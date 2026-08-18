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

/// What a draft comment is attached to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Attachment {
    /// A span of diff rows. `anchor` is what GitHub is told; `rows` is what the
    /// gutter marks and the cursor tests.
    Lines {
        rows: RangeInclusive<usize>,
        anchor: Anchor,
    },
    /// The file as a whole, for a remark that belongs to no particular line.
    File,
}

/// A review comment written but not yet submitted. Held locally so a whole
/// review can be composed offline and sent in one request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub path: String,
    pub attachment: Attachment,
    pub body: String,
}

impl Draft {
    pub const fn is_file_level(&self) -> bool {
        matches!(self.attachment, Attachment::File)
    }

    pub const fn rows(&self) -> Option<&RangeInclusive<usize>> {
        match &self.attachment {
            Attachment::Lines { rows, .. } => Some(rows),
            Attachment::File => None,
        }
    }

    pub const fn anchor(&self) -> Option<&Anchor> {
        match &self.attachment {
            Attachment::Lines { anchor, .. } => Some(anchor),
            Attachment::File => None,
        }
    }

    pub fn covers(&self, path: &str, row: usize) -> bool {
        self.path == path && self.rows().is_some_and(|rows| rows.contains(&row))
    }

    pub fn overlaps(&self, path: &str, rows: &RangeInclusive<usize>) -> bool {
        self.path == path
            && self.rows().is_some_and(|own| {
                own.start() <= rows.end() && rows.start() <= own.end()
            })
    }

    /// One entry of a review's `comments` array.
    ///
    /// GitHub anchors a span at its last line and takes the first as
    /// `start_line`, which is only sent when the comment really covers more than
    /// one. A file-level comment carries no position at all and says so with
    /// `subject_type`, which is what stops the API rejecting the missing line.
    pub fn to_api(&self) -> serde_json::Value {
        let Attachment::Lines { anchor, .. } = &self.attachment else {
            return serde_json::json!({
                "path": self.path,
                "body": self.body,
                "subject_type": "file",
            });
        };

        let mut comment = serde_json::json!({
            "path": self.path,
            "body": self.body,
            "line": anchor.end_line,
            "side": anchor.side.as_api(),
        });

        if anchor.is_multiline() {
            comment["start_line"] = anchor.start_line.into();
            comment["start_side"] = anchor.start_side.as_api().into();
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
    let rows = file.lines.get(rows).unwrap_or_default();
    let first = rows.iter().position(|line| line.kind != LineKind::Hunk)?;
    let last = rows.iter().rposition(|line| line.kind != LineKind::Hunk)?;

    // A span lives inside one hunk. Running over a header puts unrelated parts
    // of the file at either end of it, and GitHub refuses the whole review for
    // it, so the selection is refused here instead.
    let selected = &rows[first..=last];
    if selected.iter().any(|line| line.kind == LineKind::Hunk) {
        return None;
    }

    let selected: Vec<&DiffLine> = selected.iter().collect();

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
