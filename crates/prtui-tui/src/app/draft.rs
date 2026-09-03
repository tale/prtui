use prtui_core::{ChangedFile, DiffLine, LineKind, NewThread};
use std::ops::RangeInclusive;
use std::sync::Arc;

pub use prtui_core::{Anchor, Parent, Side};

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

/// How far a draft has got towards the copy GitHub holds.
///
/// A draft is drawn the moment it is written, so every state but `Synced` says
/// the screen is ahead of the server. `Queued` is written but not yet sent,
/// which is where a draft waits while the first one opens the pending review.
/// `Creating` carries `is_dirty` because a draft edited before its own creation
/// lands has no comment id to address the edit to: the follow-up is held until
/// one comes back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Sync {
    Queued,
    Creating { is_dirty: bool },
    Synced,
    Updating,
    Deleting,
    Failed(String),
}

impl Sync {
    pub const fn is_settled(&self) -> bool {
        matches!(self, Self::Synced)
    }

    /// True while the draft is out at GitHub, which is when a second request
    /// for the same one has to wait rather than race it.
    pub const fn is_in_flight(&self) -> bool {
        matches!(
            self,
            Self::Creating { .. } | Self::Updating | Self::Deleting
        )
    }

    pub const fn marker(&self) -> &'static str {
        match self {
            Self::Synced => " ✎",
            Self::Failed(_) => " !",
            _ => " ⋯",
        }
    }
}

/// A comment on the pending review GitHub holds for this pull request.
///
/// Drafts are written straight through rather than piled up locally: they
/// survive a restart, they show up on github.com, and submitting the review is
/// a verdict rather than a bulk upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// Local and monotonic. A draft is drawn before GitHub has named it, so
    /// this is what an answer is matched back against.
    pub id: u64,
    pub path: Arc<str>,
    pub attachment: Attachment,
    pub body: String,
    /// The comment node id, absent until the creation lands.
    pub remote: Option<Arc<str>>,
    pub sync: Sync,
}

impl Draft {
    /// Takes the stable fields a network task needs while the on-screen draft
    /// remains available for further edits.
    pub fn new_thread(&self, parent: Parent) -> NewThread {
        NewThread {
            parent,
            path: self.path.clone(),
            body: self.body.clone(),
            anchor: self.anchor().copied(),
        }
    }

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
        *self.path == *path
            && self.rows().is_some_and(|rows| rows.contains(&row))
    }

    pub fn overlaps(&self, path: &str, rows: &RangeInclusive<usize>) -> bool {
        *self.path == *path
            && self.rows().is_some_and(|own| {
                own.start() <= rows.end() && rows.start() <= own.end()
            })
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

/// The inverse of `anchor_for`: the rows a pending thread read back from GitHub
/// covers, which is what the gutter marks and the cursor tests.
///
/// A deletion is addressed by its old line number and everything else by its
/// new one, so which of the two a row is matched on follows the anchor's side
/// rather than the row's own kind.
pub fn rows_for(
    file: &ChangedFile,
    anchor: &Anchor,
) -> Option<RangeInclusive<usize>> {
    let matches = |line: &DiffLine, number: u32, side: Side| match side {
        Side::Left => {
            line.kind == LineKind::Removed && line.old_line == Some(number)
        }
        Side::Right => {
            line.kind != LineKind::Removed && line.new_line == Some(number)
        }
    };

    let start = file
        .lines
        .iter()
        .position(|line| matches(line, anchor.start_line, anchor.start_side))?;
    let end = file.lines[start..]
        .iter()
        .position(|line| matches(line, anchor.end_line, anchor.side))?;

    Some(start..=start + end)
}
