# prtui
> Purr-tooey

Something about a cat eventually?

## Install

```sh
brew tap tale/tap
brew install prtui
```

The formula tracks `main` and builds from source, so git needs an SSH key that
can read this repo. Pick up later commits with `brew upgrade --fetch-HEAD prtui`
— plain `brew upgrade` never re-checks a HEAD install.

Homebrew refuses source builds when Xcode is older than it wants, so the install
machine needs a current Xcode or none at all.
