use std::fmt::Write;

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
