use std::fmt::Write as _;
use std::path::Path;
use std::{env, fs};

// Splits schemes.conf into a table of names and Ghostty theme text.
fn main() {
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("schemes.conf");
    println!("cargo:rerun-if-changed={}", source.display());

    let text = fs::read_to_string(&source).expect("reading schemes.conf");
    let mut table = String::from("&[\n");
    for section in text.split("\n[").skip(1) {
        let (name, body) = section.split_once("]\n").expect("scheme header");
        let _ = writeln!(table, "    ({name:?}, {body:?}),");
    }

    table.push_str("]\n");
    let output = Path::new(&env::var("OUT_DIR").unwrap()).join("schemes.rs");
    fs::write(output, table).expect("writing scheme table");
}
