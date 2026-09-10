use anyhow::{Context, Result, bail};
use prtui_core::{ChangedFile, LineKind, parse_patch};
use prtui_tui::{
    app::App,
    renderer::{self, Theme},
    terminal,
};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::Arc,
};

use crate::{ReviewExit, ThemeChoice, viewer_loop};

struct Snapshot {
    root: PathBuf,
    files: Vec<ChangedFile>,
    blobs: HashMap<Arc<str>, Arc<[String]>>,
}

fn git(root: &Path, args: &[&str]) -> Result<Output> {
    Command::new("git")
        .current_dir(root)
        .env("GIT_LITERAL_PATHSPECS", "1")
        .args(args)
        .output()
        .context("running git")
}

fn checked(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = git(root, args)?;
    if !output.status.success() {
        bail!("git: {}", String::from_utf8_lossy(&output.stderr).trim());
    }
    Ok(output.stdout)
}

impl Snapshot {
    fn load(directory: &Path) -> Result<Self> {
        let root = checked(directory, &["rev-parse", "--show-toplevel"])
            .context("diff requires a local Git working tree")?;
        let root =
            PathBuf::from(String::from_utf8(root)?.trim_end_matches('\n'));
        let has_head = git(&root, &["rev-parse", "--verify", "HEAD"])?
            .status
            .success();
        let base = if has_head {
            "HEAD".to_string()
        } else {
            String::from_utf8(checked(
                &root,
                &["hash-object", "-t", "tree", "--stdin"],
            )?)?
            .trim()
            .to_string()
        };
        let names = checked(
            &root,
            &["diff", "--no-renames", "--name-status", "-z", &base, "--"],
        )?;
        let names =
            String::from_utf8(names).context("file paths must be UTF-8")?;
        let mut paths = Vec::new();
        let mut fields = names.split_terminator('\0');
        while let Some(status) = fields.next() {
            let path = fields.next().context("missing diff path")?;
            paths.push((
                path.to_string(),
                match status {
                    "A" => "added",
                    "D" => "removed",
                    _ => "modified",
                },
                false,
            ));
        }
        let untracked = checked(
            &root,
            &["ls-files", "--others", "--exclude-standard", "-z"],
        )?;
        let untracked =
            String::from_utf8(untracked).context("file paths must be UTF-8")?;
        paths.extend(
            untracked
                .split_terminator('\0')
                .map(|path| (path.to_string(), "added", true)),
        );
        paths.sort_by(|a, b| a.0.cmp(&b.0));

        let mut snapshot = Self {
            root,
            files: Vec::new(),
            blobs: HashMap::new(),
        };
        for (path, status, is_untracked) in paths {
            let mut args = vec![
                "diff",
                "--no-ext-diff",
                "--no-textconv",
                "--no-color",
                "--no-renames",
                "--unified=3",
            ];
            if is_untracked {
                args.extend(["--no-index", "--", "/dev/null", &path]);
            } else {
                args.extend([&base, "--", &path]);
            }
            let output = git(&snapshot.root, &args)?;
            if !(output.status.success()
                || is_untracked && output.status.code() == Some(1))
            {
                bail!(
                    "reading diff for {path}: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
            }
            let diff = String::from_utf8_lossy(&output.stdout);
            let body = diff.lines().position(|line| line.starts_with("@@ "));
            let lines = body.map_or_else(Vec::new, |index| {
                parse_patch(
                    &diff.lines().skip(index).collect::<Vec<_>>().join("\n"),
                )
            });
            let path: Arc<str> = path.into();
            let file = snapshot.root.join(&*path);
            if !file.is_symlink()
                && let Ok(content) = std::fs::read(&file)
                && !content.contains(&0)
            {
                snapshot.blobs.insert(
                    path.clone(),
                    String::from_utf8_lossy(&content)
                        .lines()
                        .map(str::to_string)
                        .collect(),
                );
            }
            snapshot.files.push(ChangedFile {
                path,
                status: status.into(),
                additions: lines
                    .iter()
                    .filter(|line| line.kind == LineKind::Added)
                    .count() as u32,
                deletions: lines
                    .iter()
                    .filter(|line| line.kind == LineKind::Removed)
                    .count() as u32,
                lines,
            });
        }
        Ok(snapshot)
    }
}

pub async fn run(choice: ThemeChoice) -> Result<()> {
    let directory = std::env::current_dir()?;
    let snapshot =
        tokio::task::spawn_blocking(move || Snapshot::load(&directory))
            .await??;
    if snapshot.files.is_empty() {
        println!("No uncommitted changes.");
        return Ok(());
    }
    let mut theme = Theme::for_mode(choice.resolve());
    let app = App::local(
        theme,
        snapshot.root.display().to_string(),
        snapshot.files,
        snapshot.blobs,
    );
    std::thread::spawn(move || renderer::preload(theme.mode));
    terminal::scope(choice.follows_terminal(), async move |terminal, events| {
        viewer_loop(
            terminal,
            events,
            &mut theme,
            choice.follows_terminal(),
            app,
            ReviewExit::Process,
            |effect, _, _| bail!("unexpected local diff effect: {effect:?}"),
        )
        .await?;
        Ok(())
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Repository(PathBuf);

    impl Repository {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "prtui-local-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&path).unwrap();
            checked(&path, &["init", "--quiet"]).unwrap();
            Self(path)
        }

        fn write(&self, path: &str, content: impl AsRef<[u8]>) {
            std::fs::write(self.0.join(path), content).unwrap();
        }

        fn commit(&self) {
            checked(&self.0, &["add", "."]).unwrap();
            checked(
                &self.0,
                &[
                    "-c",
                    "user.name=Test",
                    "-c",
                    "user.email=test@example.com",
                    "-c",
                    "commit.gpgsign=false",
                    "commit",
                    "--quiet",
                    "-m",
                    "initial",
                ],
            )
            .unwrap();
        }
    }

    impl Drop for Repository {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn combines_index_worktree_and_untracked_from_subdirectory() {
        let repo = Repository::new();
        repo.write("tracked", "original\n");
        repo.write("deleted", "gone\n");
        repo.write(".gitignore", "ignored\n");
        repo.commit();
        repo.write("tracked", "staged\n");
        checked(&repo.0, &["add", "tracked"]).unwrap();
        repo.write("tracked", "working\n");
        std::fs::remove_file(repo.0.join("deleted")).unwrap();
        repo.write("new\tfile\nname", "new\n");
        repo.write("ignored", "hidden\n");
        repo.write("binary", [0, 1, 2]);
        std::fs::create_dir(repo.0.join("sub")).unwrap();

        let snapshot = Snapshot::load(&repo.0.join("sub")).unwrap();
        assert_eq!(snapshot.files.len(), 4);
        let tracked = snapshot
            .files
            .iter()
            .find(|file| &*file.path == "tracked")
            .unwrap();
        assert_eq!((tracked.additions, tracked.deletions), (1, 1));
        assert!(tracked.lines.iter().any(|line| line.text == "working"));
        assert!(!tracked.lines.iter().any(|line| line.text == "staged"));
        assert!(
            snapshot
                .files
                .iter()
                .any(|file| &*file.path == "new\tfile\nname")
        );
        assert!(snapshot.files.iter().any(|file| file.status == "removed"));
        assert!(
            snapshot
                .files
                .iter()
                .find(|file| &*file.path == "binary")
                .unwrap()
                .lines
                .is_empty()
        );
    }

    #[test]
    fn supports_unborn_and_clean_repositories() {
        let repo = Repository::new();
        assert!(Snapshot::load(&repo.0).unwrap().files.is_empty());
        repo.write("staged", "first\n");
        checked(&repo.0, &["add", "staged"]).unwrap();
        repo.write("untracked", "second\n");
        let snapshot = Snapshot::load(&repo.0).unwrap();
        assert_eq!(snapshot.files.len(), 2);
        assert!(
            snapshot
                .files
                .iter()
                .all(|file| file.status == "added" && file.additions == 1)
        );
        repo.commit();
        assert!(Snapshot::load(&repo.0).unwrap().files.is_empty());
    }

    #[test]
    fn parses_diff_and_existing_pr_invocations() {
        use clap::Parser;
        assert!(matches!(
            crate::Args::try_parse_from(["prtui", "diff", "--theme", "dark"])
                .unwrap()
                .command,
            Some(crate::Command::Diff)
        ));
        assert_eq!(
            crate::Args::try_parse_from(["prtui", "123", "-R", "owner/repo"])
                .unwrap()
                .number,
            Some(123)
        );
    }
}
