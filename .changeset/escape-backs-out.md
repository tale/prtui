---
default: patch
---

#### `<Esc>` no longer quits

It backs out of a conversation, then a live search, and then says how to leave.
Quitting outright is the one thing nobody means by the key they press to escape
a state they did not want. `q`, `:q` and `<C-c>` are unchanged.
