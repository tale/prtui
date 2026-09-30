# Changelog

## 0.4.1 (2026-09-30)

### Features

- The review diff can be narrowed to one commit, a range of commits, or the changes since your last review from the commit panel (`L`).
- Scrolling past either end of a file in the diff continues into the next or previous file.
- The PR overview now lists submitted reviews with their decision in chronological order.

### Fixes

- Key hints, the key reference, and status messages use the same name for each action on every screen.
- UI components of the application now follow consistent styling throughout the review screen and PR list.
- Branches, authors, dates, line counts, and unresolved thread counts are all formatted the same across the application.
- Commenting (`c`) on a draft reopens it, and commenting from the file tree writes a file note.
- The cursor stays visible on diff hunk headers and on range-selected commits in the commit panel.
- The documentation site covers installation, review workflows, local diffs, keyboard shortcuts, and configuration.
- Hidden lines in a diff can be expanded with the Enter key or `E` to expand the entire file.
- Thread markers in the file tree, diff title, and status bar count only open threads, leaving out outdated ones, and folded directories without threads show no marker.
- `:e`/`:reload` can refresh the data in the PR overview and review UI.
- Commenting (`c`) on a line with an open thread replies to that thread.
- Panel key hints appear only in the status bar, and the diff's status bar keeps its navigation hints while drafts are pending and shows visual mode hints when reviewing local changes.
- The keybind reference panel groups keys by tasks, significantly simplifying the listing.
- The file tree shows and hides with `:tree` instead of `f`.
- Resolved GitHub review threads can be reopened with `R`.

## 0.4.0 (2026-09-16)

### Features

- GitLab merge requests can be reviewed through the new `gitlab` provider, on gitlab.com and on self-hosted instances.
- The `diff` command displays staged, unstaged, and untracked local changes.

### Fixes

- Diff line count changes now show in the PR selector.
- GitHub pull requests are now fetched via the GitHub GraphQL API.
- Nix builds provide Git when running tests.
- Nix packages support Intel macOS with the stable 26.05 package set.
- Nix packages report the version from the release manifest.
- Repository discovery uses Git remotes and remembers positively identified providers per host.
- Renamed files show their original and current paths in the diff header.

## 0.3.0 (2026-09-04)

### Breaking Changes

- Split the library API into core, TUI, and GitHub provider crates.

### Features

- Edit every free-text prompt with readline commands such as `<C-a>` and `<C-e>`.
- `K` opens a Vim-style PR summary with individually folded comments.
- The file tree now wraps around when you go past the end of it.

### Fixes

- Show which pane is focused with an emphasized title and cut off cursor line.
- Load the full diff for large files that GitHub doesn't send by default.

## 0.2.0 (2026-08-30)

### Breaking Changes

- `prtui` opens a pull request dashboard and returns to it after each review

### Features

- `x` marks the open file as viewed on GitHub and opens the next unread one
- `o` opens the pull request description and comments
- `/` searches the description and the `?` key list
- `y` copies a permalink, `gx` opens the pull request in a browser
- `<C-p>` and `<C-n>` bring back earlier searches, filters and commands

### Fixes

- `<Esc>` clears a search from the file tree, not just from the diff
- `<Esc>` no longer quits prtui
- Reviews with over 100 files, conversations or comments are no longer cut short
- GitHub credentials are never sent to cross-origin pagination links
- `/` starts a new filter instead of editing the last one
- Stale syntax colors are discarded after files or terminal themes change
- GitHub Enterprise uses its own `gh` login, not your github.com token

## 0.1.0 (2026-08-26)

Initial release.
