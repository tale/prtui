---
title: Review a pull request
description: Navigate diffs, write comments, follow threads, and publish a review.
---

# Review a pull request

Open a request with `prtui` or `prtui 123`. The review workflow is the same
across supported code hosts.

## Read the diff

The file tree and diff are separate panes. Use `h` / `l` to focus them, or `Tab`
to switch. Move with `j` / `k`; use `]` / `[` to open the next or previous file.
Type `:tree` to hide or show the tree.

Press `/` to filter the tree or search the current diff. `Enter` keeps the search;
`Esc` cancels it. Use `n` / `N` for the next or previous match and `:noh` to clear it.

Press `Enter` on a folded region to reveal context, or `E` to expand the whole
file. Press `K` for the description, review history, and discussion.

## Write a comment

`c` acts on what you are reading:

| Cursor location | Action                       |
| --------------- | ---------------------------- |
| Code line       | Start a line comment         |
| Selected lines  | Start a comment on the range |
| File tree       | Start a file comment         |
| Existing thread | Reply to the thread          |
| Your draft      | Edit the draft               |

To select a range, press `v`, move with `j` / `k`, then press `c`. Keep the range
within one diff hunk. `C` starts a file comment directly from the diff.

In the composer, `Enter` saves and `Alt+Enter` or `Ctrl+j` inserts a newline.
`Esc` closes the composer; follow the discard prompt if you have unsaved text.
Use `c` to reopen a draft or `d` to delete the draft under the cursor.

Saved drafts sync to the host and return when you reopen the request. Wait for
sync to finish before quitting; an error marker means the draft has not synced.

**Replies post immediately.** They do not wait for review submission. Resolving
or reopening a thread also takes effect immediately.

## Follow threads

Use `}` / `{` to jump to the next or previous open thread. `Enter` opens or
collapses the thread under the cursor. Press `c` to reply, `R` to resolve or reopen
it, and `Esc` to return focus to the code.

`gx` opens the current context in your browser. `y` copies a permalink to the
file, selected lines, or comment.

## Review specific commits

<video controls playsinline preload="none" poster="./commit-diffs.webp" width="2304" height="1440" aria-label="Watch how to select a commit or commit range to review">
  <source src="./commit-diffs.mp4" type="video/mp4">
  Read the commit selection instructions below if your browser cannot play this video.
</video>

Press `L` to choose which changes to read:

- **All changes** shows the full request diff.
- **Since your last review** shows changes after your last reviewed commit, when available.
- Select a commit and press `Enter` to read that commit.
- Press `v` on a commit, move to another, then `Enter` to read the range.

Commit-scoped diffs are read-only. Return to **All changes** with `L` before
commenting, managing threads, or marking files viewed.

## Submit the review

Press `x` to toggle the current file's viewed mark and advance to the next
unviewed file. GitHub saves viewed marks; GitLab does not persist them through
its API.

Press `s` or type `:w` to open the review form. Use `Tab` / `Shift+Tab` to choose
**Comment**, **Approve**, or **Request changes**. Add an optional summary and press
`Enter` to publish the review and its drafts. `Esc` closes the form.

If submission fails, the form shows the error. On GitLab, publication and approval
are separate steps; check the merge request before retrying a partially completed
submission.

`q` returns to the picker when you opened the request there, or exits a directly
opened request. `Ctrl+c` exits prtui.
