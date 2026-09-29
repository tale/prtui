# prtui

A modal terminal UI for reviewing GitHub pull requests and GitLab merge
requests, built around Vim motions and a command line.

![prtui reviewing a pull request](https://raw.githubusercontent.com/tale/prtui/main/docs/demo.gif)

## Install

```sh
brew install tale/tap/prtui
mise use -g github:tale/prtui@latest
cargo install prtui
nix run github:tale/prtui
```

Prebuilt binaries are attached to [releases][releases]. Reviewing needs the
host's CLI logged in: [`gh`][gh] for GitHub or [`glab`][glab] for GitLab.

## Usage

```sh
prtui              # pick an open pull request
prtui 123          # open one directly
prtui -R owner/repo 123
prtui diff         # review local changes
```

Press `?` in the app for the keys.

## Contributing

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

`mise run check` runs all three. A change users would notice needs a changeset
in `.changeset/` (`mise exec -- knope document-change`).

## License

MIT. See `LICENSE`.

[gh]: https://cli.github.com
[glab]: https://gitlab.com/gitlab-org/cli
[releases]: https://github.com/tale/prtui/releases
