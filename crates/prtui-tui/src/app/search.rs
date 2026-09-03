use crate::app::Card;
use std::ops::Range;

/// One hit for the active query, identified the way the cursor addresses it:
/// a diff row, plus the card under that row when the hit is in a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    Line(usize),
    Card { row: usize, card: Card },
}

impl Match {
    pub const fn row(&self) -> usize {
        match self {
            Self::Line(row) | Self::Card { row, .. } => *row,
        }
    }

    /// Cloning a card is a refcount bump at worst, so a caller that has to
    /// outlive the borrow takes one rather than copying the string.
    pub fn card(&self) -> Option<Card> {
        match self {
            Self::Line(_) => None,
            Self::Card { card, .. } => Some(card.clone()),
        }
    }
}

/// What the reader typed, with its case rule already decided.
///
/// Every search in the app runs through this: the diff, the comments and the
/// file tree. Matching allocates nothing, since the tree tests it against every
/// path in the pull request on each keystroke.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query<'a> {
    needle: &'a str,
    is_case_sensitive: bool,
}

impl<'a> Query<'a> {
    /// Vim's smartcase: an all-lowercase query ignores case, and a single
    /// uppercase character makes the whole query case-sensitive. An empty
    /// query is no query, which is what stops it matching everything.
    pub fn new(needle: &'a str) -> Option<Self> {
        (!needle.is_empty()).then(|| Self {
            needle,
            is_case_sensitive: needle.chars().any(char::is_uppercase),
        })
    }

    pub fn is_match(&self, haystack: &str) -> bool {
        self.find_from(haystack, 0).is_some()
    }

    /// Byte ranges of every occurrence, for painting the hits.
    pub fn ranges(&self, haystack: &str) -> Vec<Range<usize>> {
        let mut ranges = Vec::new();
        let mut from = 0;

        while let Some(start) = self.find_from(haystack, from) {
            let end = start + self.needle.len();
            ranges.push(start..end);
            from = end;
        }

        ranges
    }

    /// Case folding is ASCII-only, so a non-ASCII pair matches only when typed
    /// exactly. UTF-8 is self-synchronizing, so a byte-wise hit can only begin
    /// on a scalar boundary and the offsets stay valid for slicing.
    fn find_from(&self, haystack: &str, from: usize) -> Option<usize> {
        if self.is_case_sensitive {
            return haystack
                .get(from..)?
                .find(self.needle)
                .map(|offset| from + offset);
        }

        let needle = self.needle.as_bytes();
        let haystack = haystack.as_bytes();
        let last = haystack.len().checked_sub(needle.len())?;

        (from..=last).find(|&start| {
            haystack[start..start + needle.len()].eq_ignore_ascii_case(needle)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(haystack: &str, needle: &str) -> Vec<Range<usize>> {
        Query::new(needle).map_or_else(Vec::new, |query| query.ranges(haystack))
    }

    #[test]
    fn an_uppercase_character_makes_the_whole_query_case_sensitive() {
        assert!(Query::new("todo").unwrap().is_match("TODO: fix"));
        assert!(!Query::new("Todo").unwrap().is_match("TODO: fix"));
        assert!(Query::new("TODO").unwrap().is_match("TODO: fix"));
    }

    #[test]
    fn an_empty_query_is_no_query() {
        assert!(Query::new("").is_none());
    }

    #[test]
    fn every_occurrence_is_reported_without_overlapping() {
        assert_eq!(ranges("ababab", "ab"), [0..2, 2..4, 4..6]);
        assert_eq!(ranges("aaaa", "aa"), [0..2, 2..4]);
        assert!(ranges("nothing here", "zz").is_empty());
    }

    /// Offsets index the original bytes, so a hit past a multi-byte scalar has
    /// to still slice.
    #[test]
    fn offsets_stay_on_scalar_boundaries() {
        let haystack = "café au LAIT";

        let hits = ranges(haystack, "lait");
        assert_eq!(hits.len(), 1);
        assert_eq!(&haystack[hits[0].clone()], "LAIT");

        // A non-ASCII pair only matches when it was typed exactly.
        assert!(ranges("CAFÉ", "café").is_empty());
        assert_eq!(ranges("café", "café").len(), 1);
    }
}
