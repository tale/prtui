---
title: Get started
description: Install prtui, connect your code host, and start a terminal review.
---

# Get started

Review pull requests and local changes from the terminal. Navigate with Vim motions, read discussions beside the diff, and submit
your review without leaving the keyboard.

## Install

Choose one:

::: code-group

```sh [Homebrew]
brew install tale/tap/prtui
```

```sh [mise]
mise use -g github:tale/prtui@latest
```

```sh [Cargo]
cargo install prtui
```

```sh [Nix]
nix run github:tale/prtui
```

:::

Or download a binary from [releases](https://github.com/tale/prtui/releases).

## Authenticate

Install the CLI for your host and sign in. prtui uses its credentials.

| Host   | CLI                                       | Sign in           |
| ------ | ----------------------------------------- | ----------------- |
| GitHub | [gh](https://cli.github.com)              | `gh auth login`   |
| GitLab | [glab](https://gitlab.com/gitlab-org/cli) | `glab auth login` |

[Local diffs](./local-diffs) only need Git, with no host login.

## Open a review

From a Git repository:

```sh
prtui                  # pick an open pull or merge request
prtui 123              # open request 123
prtui -R owner/repo 123 # use another repository
prtui diff             # inspect local changes
```

In the picker, use `j` / `k` to move, `/` to filter, and `Enter` to open.
See [CLI & configuration](./cli) for repository namespaces and self-hosted instances.

## Your first review

1. Press `l` to focus the diff; move with `j` / `k` and switch files with `]` / `[`.
2. Press `c` on a line to write a comment, then `Enter` to save the draft.
3. Press `x` to mark a file viewed and move to the next unviewed file.
4. Press `s` to open the review form. Choose a verdict with `Tab`, add a summary,
   and press `Enter` to publish.

Press `?` for keys in the app. The [review guide](./reviewing) covers selections,
threads, drafts, and reviewing individual commits.
