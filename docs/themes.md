---
title: Themes
description: Color prtui with a bundled Base16 scheme, or pair one for light and dark terminals.
---

<script setup>
import SchemeGallery from './.vitepress/theme/SchemeGallery.vue'
</script>

# Themes

prtui colors code, diffs, and the interface from one Base16 scheme. The default
`auto` theme uses GitHub's light or dark colors to match the terminal background,
and switches when the background changes.

## Choose a theme

Pass a scheme name to `--theme` to use it on any background:

```sh
prtui --theme catppuccin-mocha
```

To follow the terminal between light and dark, give one scheme for each:

```sh
prtui --theme light:catppuccin-latte,dark:catppuccin-mocha
```

Names ignore case. prtui adjusts colors that sit too close to the background, so
text and diff rows stay readable in every scheme.

## Bundled schemes

Select a name to copy its `--theme` flag. The schemes come from
[tinted-theming/schemes](https://github.com/tinted-theming/schemes); their authors
and license are listed in
[SCHEMES.md](https://github.com/tale/prtui/blob/main/crates/prtui-tui/SCHEMES.md).

<SchemeGallery />
