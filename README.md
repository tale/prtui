# prtui

A modal terminal UI for reviewing GitHub pull requests for the people who are
most comfortable in the terminal with Vim motions and a command line.

![prtui reviewing a pull request](https://raw.githubusercontent.com/tale/prtui/main/docs/demo.gif)

## Install

Any of these work. All of them need the [GitHub CLI][gh] on your `PATH`, which
is where `prtui` gets its credentials and works out which repo you are in.

**Homebrew**

```sh
brew install tale/tap/prtui
```

**Cargo**

```sh
cargo install prtui
```

**Nix**

```sh
nix run github:tale/prtui -- 1234
```

**Prebuilt binaries** for macOS and Linux, on arm64 and x86_64, are attached to
every [release][releases] with SHA-256 checksums. Linux builds use glibc 2.35 so
they require Ubuntu 22.04, Debian 12, RHEL 9, or newer.

## Use

Authenticate once, then open a pull request by number:

```sh
gh auth login
prtui 1234
```

From inside a checkout, the repo is determined based on the git remote of the
current working directory. You can also override it with `-R` like below:

```sh
prtui 1234 -R rust-lang/cargo
```

GitHub Enterprise works through the same flag, with the host in front:

```sh
gh auth login --hostname github.example.com
prtui 1234 -R github.example.com/team/service
```

Each host needs its own `gh` login; `prtui` uses the token for the host it is
reviewing and never sends one host's credential to another.

```
Options:
  -R, --repo <[HOST/]OWNER/REPO>  Select another repository
      --theme <auto|dark|light>   Color theme [default: auto]
  -h, --help                      Print help
  -V, --version                   Print version
```

## Keys

Press `?` for the same list in the app, or `:` for a command line. Every key
below is a named command, so anything bound to a key is also reachable as
`:name` — and `:42` jumps to line 42.

**The pull request** — `o` opens the description and the discussion over the
panes, and `?` opens the key reference. Both scroll with the same keys the rest
of the app uses, both are searchable with `/`, and `esc` closes either.

**Motion** — `j`/`k` a row, `<C-d>`/`<C-u>` half a screen, `gg`/`G` the first
and last line. A count works where you would expect: `10j`.

**Files** — `]`/`[` step through files, `f` shows or hides the tree, `<Tab>`
swaps the focused pane, `h`/`l` move between them, `<CR>` opens what the cursor
is on.

**Reading** — `/` searches whatever you are reading: an open panel, the tree,
or the file, and starts clean each time. `n`/`N` walk the hits and `:noh`
clears them. Inside any prompt — `/` or `:` — the arrows step what is under it
and `<C-p>`/`<C-n>` recall what you typed there before. `za` reveals the
hidden lines under the cursor, `zj`/`zk` reveal downward or upward, and `zR`
opens every gap in the file — the surrounding code is fetched from GitHub on
demand.

**Conversations** — `}`/`{` jump between unanswered threads and `R` resolves
or reopens the one you are on.

**Comments** — `c` comments on the line, on a visual span, or replies to the
thread under the cursor. `v` selects lines first; `C` writes a note about the
whole file. `e` reopens a draft, `d` discards it.

**Links** — `y` copies a permalink to whatever the cursor is on: the line, the
visual span, or the conversation. The copy goes through the terminal itself, so
it works over SSH. `gx` opens the pull request in a browser.

**Submitting** — `s` opens the form. `<Tab>` steps the verdict between
comment, approve, and request changes; `<CR>` ships every draft as one review.

`q` or `:q` quits. `<Esc>` backs out of whatever you are inside — a
conversation, then a live query — but never out of the app.

## Contributing

CI runs formatting, Clippy (pedantic, nursery, and cargo lints, warnings denied)
and the test suite. Reproduce all three locally:

```sh
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
```

With [mise][mise] installed, `mise run check` does the same and `mise install`
sets up the pre-commit hook that formats and lints staged Rust.

`ARCHITECTURE.md` describes the internal boundaries and the refactors still in
flight; read it before a change that crosses modules.

A change someone using `prtui` would notice needs a changeset: `mise exec --
knope document-change` writes one into `.changeset/`, saying what the change
gives them rather than what it altered. Internal work needs none.

Keep the entry to one physical line, under 78 columns. knope reads the first
line as a title and everything after it as a body, so wrapping turns a bullet
into a heading and splits the sentence wherever the wrap fell.

Write it the way a release note reads, not the way a commit message does: what
the tool now does, in the words the keys and the panes already go by. `` `o`
opens the pull request description ``, not `Add an overview overlay`.

## License

MIT. See `LICENSE`.

[gh]: https://cli.github.com
[mise]: https://mise.jdx.dev
[releases]: https://github.com/tale/prtui/releases
