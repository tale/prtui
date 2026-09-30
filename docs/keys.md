---
title: Keyboard reference
description: The keys for moving, reading diffs, commenting, and submitting reviews.
---

# Keyboard reference

Keys are case-sensitive. Chords such as `gg` and `gx` are typed in sequence.
Press `?` in prtui for the reference available in your current mode.

## Move & search

| Key                  | Action                                         |
| -------------------- | ---------------------------------------------- |
| `j` / `k`, `↓` / `↑` | Move down / up                                 |
| `Ctrl+d` / `Ctrl+u`  | Move half a screen down / up                   |
| `gg` / `G`           | First / last line                              |
| `h` / `l`, `←` / `→` | Focus file tree / diff                         |
| `Tab`                | Switch panes                                   |
| `]` / `[`            | Next / previous file                           |
| `}` / `{`            | Next / previous open thread                    |
| `/`                  | Search the panel or diff; filter the file tree |
| `n` / `N`            | Next / previous match                          |

Prefix motions with a count: `10j` moves ten rows, `3]` advances three files,
and `42G` jumps to line 42.

## Read

| Key     | Action                                               |
| ------- | ---------------------------------------------------- |
| `Enter` | Open or expand the item under the cursor             |
| `E`     | Reveal all hidden lines in the file                  |
| `K`     | Open description, review history, and discussion     |
| `L`     | Select all changes, changes since review, or commits |
| `gx`    | Open the current context in a browser                |
| `y`     | Copy a permalink                                     |
| `?`     | Open the key reference                               |
| `Esc`   | Leave the thread, selection, search, or panel        |

In a panel, `q` closes it. In the commits panel, `v` starts a range selection;
move to its end and press `Enter` to apply it.

## Respond & finish

| Key       | Action                                        |
| --------- | --------------------------------------------- |
| `c`       | Comment, reply, or edit a draft at the cursor |
| `C`       | Write a file comment                          |
| `v` / `V` | Select lines for a comment                    |
| `e`       | Edit the draft under the cursor               |
| `d`       | Delete the draft under the cursor             |
| `R`       | Resolve or reopen a thread                    |
| `x`       | Toggle viewed and open the next unviewed file |
| `s`       | Open the review submission form               |
| `q`       | Leave the review                              |
| `Ctrl+c`  | Exit prtui                                    |

## Composer & review form

| Key                    | Action                                       |
| ---------------------- | -------------------------------------------- |
| `Enter`                | Save a comment; publish from the review form |
| `Alt+Enter` / `Ctrl+j` | Insert a newline                             |
| `Tab` / `Shift+Tab`    | Next / previous verdict in the review form   |
| `Esc`                  | Close; confirm discard when prompted         |
| `Ctrl+a` / `Ctrl+e`    | Start / end of the line                      |
| `Ctrl+w`               | Delete back to the previous blank            |
| `Ctrl+u` / `Ctrl+k`    | Delete to the start / end of the line        |

## Command line

Press `:`, type a command, then `Enter`. `Esc` cancels. `↑` / `↓` recall commands.

| Command          | Action                                |
| ---------------- | ------------------------------------- |
| `:tree`          | Show or hide the file tree            |
| `:42` / `:$`     | Jump to line 42 / the last line       |
| `:noh`           | Clear the search or filter            |
| `:w` / `:submit` | Open the review form                  |
| `:h` / `:help`   | Open the key reference                |
| `:o` / `:open`   | Open the current context in a browser |
| `:y` / `:yank`   | Copy a permalink                      |
| `:q` / `:quit`   | Leave the review                      |

Named actions also work as commands, for example `:next-file`, `:overview`,
`:commits`, and `:expand-file`.
