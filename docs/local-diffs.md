---
title: Review local changes
description: Inspect staged, unstaged, and untracked changes before opening a pull request.
ogImage: /og/local-diffs.jpg
ogImageAlt: prtui displaying local file changes and their staging state
---

# Review local changes

Run inside a Git repository:

```sh
prtui diff
```

prtui shows a snapshot of staged, unstaged, and untracked files. No code-host login is needed.

## Read staging state

The file tree marks where each file has changes:

| Marker | State                    |
| ------ | ------------------------ |
| `S`    | Staged                   |
| `U`    | Unstaged                 |
| `SU`   | Both staged and unstaged |
| `?`    | Untracked                |

The diff shows the combined change from `HEAD` to the working tree, rather than
separate staged and unstaged patches. A file can appear in the tree with no net
diff if its staged and unstaged changes cancel each other out.

## Navigate

<video controls playsinline preload="none" poster="./local-diffs.webp" width="2304" height="1440" aria-label="Watch how to browse and filter local changes">
  <source src="./local-diffs.mp4" type="video/mp4">
  Read the local changes instructions below if your browser cannot play this video.
</video>

Use `h` / `l` to switch panes, `j` / `k` to move, and `]` / `[` to switch files.
Press `/` to filter files or search the diff, `Enter` to expand context, and `E`
to reveal the full file. `:tree` toggles the file tree; `?` shows available keys.

Local mode is a viewer: it does not stage files or submit comments. Reopen
`prtui diff` to load changes made since the snapshot. Press `q` to exit.
