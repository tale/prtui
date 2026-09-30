---
title: CLI & configuration
description: Repository selection, providers, authentication, and terminal themes.
---

# CLI & configuration

```text
prtui [OPTIONS] [NUMBER]
prtui diff [--theme auto|dark|light]
```

Without a number, prtui opens the request picker. Inside a repository it uses the
Git remote, preferring `origin`. Outside a repository it opens the host's request
listing for your account.

## Options

| Option                               | Purpose                                  |
| ------------------------------------ | ---------------------------------------- |
| `-R`, `--repo [HOST/]NAMESPACE/REPO` | Select a repository                      |
| `--provider github\|gitlab`          | Override provider detection              |
| `--theme auto\|dark\|light`          | Choose a color theme; defaults to `auto` |
| `-h`, `--help`                       | Show command help                        |
| `-V`, `--version`                    | Show the version                         |

## Repositories & hosts

For GitHub, `owner/repo` is enough. Include the host for GitLab or a self-hosted
instance; GitLab namespaces can contain subgroups.

```sh
prtui -R tale/prtui
prtui -R gitlab.com/group/subgroup/project 42
```

prtui detects the provider from the host. If detection fails, choose it explicitly:

```sh
prtui --provider gitlab -R git.example.com/group/project 42
```

Authenticate with the same host you pass to prtui:

```sh
gh auth login --hostname github.example.com
glab auth login --hostname gitlab.example.com
```

Detected providers are cached in `$XDG_CONFIG_HOME/prtui/hosts.json`, or
`~/.config/prtui/hosts.json` when `XDG_CONFIG_HOME` is unset. `--provider` overrides
that cache. If a saved provider is wrong, remove its entry to detect it again.

## Appearance

The default `auto` theme reads the terminal background and follows theme changes.
Override it when needed:

```sh
prtui --theme light
prtui diff --theme dark
```

## Troubleshooting

**Repository not found:** check the remote with `git remote -v`, or pass
`-R [HOST/]NAMESPACE/REPO` explicitly. Include the host for self-hosted instances.

**Authentication or permission error:** confirm `gh` or `glab` is on your `PATH`
and signed in to the correct host with access to the repository. Use
`gh auth status` or `glab auth status` to check the login.

**Cannot comment:** local diffs and selected commit ranges are read-only. For a
remote review, press `L` and select **All changes** before writing a comment.

**Unexpected colors:** try `--theme dark` or `--theme light`.
