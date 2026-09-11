use std::fmt::Write;

pub fn escape_segment(text: &str) -> String {
    escape(text, b"-._~")
}

pub fn escape_path(path: &str) -> String {
    escape(path, b"/-._~")
}

fn escape(text: &str, keep: &[u8]) -> String {
    let mut escaped = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || keep.contains(&byte) {
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
    fn a_repository_path_encodes_to_one_segment() {
        assert_eq!(
            escape_segment("group/sub/project"),
            "group%2Fsub%2Fproject"
        );
        assert_eq!(escape_path("src/a b.rs"), "src/a%20b.rs");
    }
}
