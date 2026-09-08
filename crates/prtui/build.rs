use std::{env, path::Path, process::Command};

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let value = String::from_utf8(output.stdout).ok()?;
    Some(value.trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-env-changed=PRTUI_VERSION");
    println!("cargo:rerun-if-changed=build.rs");
    if let Ok(version) = env::var("PRTUI_VERSION") {
        assert!(
            !version.trim().is_empty(),
            "PRTUI_VERSION must not be empty"
        );
        println!("cargo:rustc-env=PRTUI_VERSION={version}");
        return;
    }

    let package_version =
        env::var("CARGO_PKG_VERSION").expect("Cargo sets the package version");

    let manifest_dir = env::var("CARGO_MANIFEST_DIR")
        .expect("Cargo sets the manifest directory");

    let root = Path::new(&manifest_dir).join("../..");
    let mut version = package_version.clone();

    if root.join(".git").exists() {
        for name in ["HEAD", "refs", "packed-refs"] {
            if let Some(path) = git(&root, &["rev-parse", "--git-path", name]) {
                println!(
                    "cargo:rerun-if-changed={}",
                    root.join(path).display()
                );
            }
        }

        if let Some(sha) = git(&root, &["rev-parse", "--short=7", "HEAD"]) {
            let tag = git(
                &root,
                &["describe", "--tags", "--abbrev=0", "--match", "v[0-9]*"],
            );

            let base = tag.as_deref().map_or(package_version.as_str(), |tag| {
                tag.trim_start_matches('v')
            });

            version = format!("{base}+{sha}");
        }
    }

    println!("cargo:rustc-env=PRTUI_VERSION={version}");
}
