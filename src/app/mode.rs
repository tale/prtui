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
}

impl Mode {
    pub fn label(self) -> &'static str {
        match self {
            Mode::Normal => " NORMAL ",
            Mode::Visual => " VISUAL ",
            Mode::Insert => " INSERT ",
            Mode::Filter => " FILTER ",
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
    pub fn at(row: usize) -> Self {
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
