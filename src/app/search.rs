use std::ops::Range;

/// One hit for the active query, identified the way the cursor addresses it:
/// a diff row, plus the thread under that row when the hit is in a comment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Match {
    Line(usize),
    Thread { row: usize, id: String },
}

impl Match {
    pub const fn row(&self) -> usize {
        match self {
            Self::Line(row) | Self::Thread { row, .. } => *row,
        }
    }

    pub fn thread_id(&self) -> Option<&str> {
        match self {
            Self::Line(_) => None,
            Self::Thread { id, .. } => Some(id),
        }
    }
}

/// Vim's smartcase: an all-lowercase query ignores case, and a single uppercase
/// character makes the whole query case-sensitive.
fn is_case_sensitive(query: &str) -> bool {
    query.chars().any(char::is_uppercase)
}

/// Byte ranges of every occurrence of `query` in `haystack`.
///
/// Case folding is ASCII-only so the returned offsets stay aligned with the
/// original bytes; non-ASCII case pairs match only when typed exactly.
pub fn ranges(haystack: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }

    if is_case_sensitive(query) {
        return haystack
            .match_indices(query)
            .map(|(start, hit)| start..start + hit.len())
            .collect();
    }

    let needle = query.to_ascii_lowercase();

    haystack
        .to_ascii_lowercase()
        .match_indices(&needle)
        .map(|(start, _)| start..start + needle.len())
        .collect()
}

pub fn is_match(haystack: &str, query: &str) -> bool {
    if query.is_empty() {
        return false;
    }

    if is_case_sensitive(query) {
        return haystack.contains(query);
    }

    haystack
        .to_ascii_lowercase()
        .contains(&query.to_ascii_lowercase())
}

pub fn probe_fix(values: &[String]) -> Vec<String> {
    values.to_vec()
}

const fn probe_fmt() -> u8 {
    7
}
