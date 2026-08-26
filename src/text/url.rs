//! Percent-encoding for the one place a path becomes part of a URL.

use std::fmt::Write;

/// Percent-encodes what a path may not carry into a URL. The separators stay:
/// a path travels as a path, not as one escaped segment.
pub fn escape_path(path: &str) -> String {
    const KEEP: &[u8] = b"/-._~";

    let mut escaped = String::with_capacity(path.len());
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || KEEP.contains(&byte) {
            escaped.push(byte as char);
            continue;
        }

        let _ = write!(escaped, "%{byte:02X}");
    }

    escaped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn separators_survive_and_everything_else_is_escaped() {
        assert_eq!(escape_path("src/app/mod.rs"), "src/app/mod.rs");
        assert_eq!(escape_path("a b.rs"), "a%20b.rs");
        assert_eq!(escape_path("q?#.rs"), "q%3F%23.rs");
    }

    /// A multi-byte scalar escapes one byte at a time, which is what UTF-8 in a
    /// URL is.
    #[test]
    fn non_ascii_escapes_by_byte() {
        assert_eq!(escape_path("café.rs"), "caf%C3%A9.rs");
    }
}
