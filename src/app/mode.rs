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
    /// Typing a `:` command.
    CommandLine,
    /// Choosing a verdict and writing the summary that ships the review.
    Submit,
}

impl Mode {
    /// The letter the keymap table names this mode by, the way Vim's `nmap`
    /// and `imap` name theirs.
    pub const fn from_letter(letter: char) -> Option<Self> {
        Some(match letter {
            'n' => Self::Normal,
            'v' => Self::Visual,
            'i' => Self::Insert,
            'f' => Self::Filter,
            's' => Self::Search,
            'c' => Self::CommandLine,
            'r' => Self::Submit,
            _ => return None,
        })
    }

    /// Where a digit is a count prefix rather than something to type.
    pub const fn takes_count(self) -> bool {
        matches!(self, Self::Normal | Self::Visual)
    }

    /// Whether the mode is editing a line of text of its own.
    pub const fn is_prompt(self) -> bool {
        matches!(
            self,
            Self::Insert
                | Self::Filter
                | Self::Search
                | Self::CommandLine
                | Self::Submit
        )
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => " NORMAL ",
            Self::Visual => " VISUAL ",
            Self::Insert => " INSERT ",
            Self::Filter => " FILTER ",
            Self::Search => " SEARCH ",
            Self::CommandLine => " COMMAND ",
            Self::Submit => " SUBMIT ",
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
