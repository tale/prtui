# Changelog

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
