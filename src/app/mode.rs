use std::ops::RangeInclusive;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Line-wise selection over diff rows. Character-wise selection has no
    /// meaning here: review comments anchor to whole lines.
    Visual,
    /// Composing a comment body; keys belong to the editor widget.
    Insert,
    /// Editing the file tree's path filter.
    Filter,
    /// Typing a query against the open file's code and comments.
    Search,
}

impl Mode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => " NORMAL ",
            Self::Visual => " VISUAL ",
            Self::Insert => " INSERT ",
            Self::Filter => " FILTER ",
            Self::Search => " SEARCH ",
        }
    }
}

/// An inclusive span of diff rows, anchored where visual mode began.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub anchor: usize,
    pub head: usize,
}

impl Selection {
    pub const fn at(row: usize) -> Self {
        Self {
            anchor: row,
            head: row,
        }
    }

    pub fn range(&self) -> RangeInclusive<usize> {
        let low = self.anchor.min(self.head);
        let high = self.anchor.max(self.head);

        low..=high
    }

    pub fn contains(&self, row: usize) -> bool {
        self.range().contains(&row)
    }

    pub fn row_count(&self) -> usize {
        let range = self.range();

        range.end() - range.start() + 1
    }
}
