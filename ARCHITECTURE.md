# Architecture

Where code lives, what the boundaries are, and which of them exist yet. Steps A
and B of the order of work have landed and describe the tree as it stands; C
through E are planned, so parts of what follows are a target rather than a
description. Every section marks which is which.

**Rules** is the contract, **Tree** is the map, and **Why** is the reasoning —
a record of the boundary problems that motivated the work, with the resolved
ones marked so the argument still reads in order.

## Why

The code is disciplined line by line: guard clauses, why-only comments, no
placeholders. What it lacks is boundaries. Several abstractions are missing,
and every place that needed one worked around it locally instead, so the same
logic now exists three times in three shapes. The defects below are all
symptoms of that, not of careless code.

### The load-bearing defect: render mutates — *resolved (A, B)*

`ui::draw(frame, app: &mut App)` takes the model mutably.
`sync_expanded_thread_scroll` writes `app.thread_scroll_limit` and clamps
`app.thread_scroll` in the middle of a render pass. Drawing is a side effect,
so layout facts have to be smuggled back into the model to be usable next
frame. Most of what follows hangs off this.

### Layout is computed twice per frame — *resolved (A)*

`draw_diff` builds fully styled thread rows purely to measure their height,
then builds them again to draw them. `sync_expanded_thread_scroll` renders a
whole thread's markdown to take `.len()`. The abandoned refactor is admitted
in a comment:

```rust
// `diff_scroll` remains source-line based until the virtual-row work in
// slice 2, but a comment block can no longer push the cursor off screen.
```

### `viewport_height` threaded through 42 call sites — *resolved (B)*

The model cannot act without layout facts, so an untyped `usize` follows every
action down the call chain. `main.rs` independently hardcodes the chrome height
as `area.height - 3`, duplicating knowledge that lives in `ui::draw`'s layout
constraints.

### Three effect channels, three shapes

`app.take_requests()` is the right pattern. But images carry a parallel
mini-outbox (`images.take_pending()`), highlighting is spawned directly from
`main` on a raw thread, and refetch-after-write is decided inside a `select!`
arm rather than by the app. Nothing arbitrates between them.

That last one used to be a live race: every successful write spawned a metadata
fetch, so resolving two threads quickly put two in flight and the older response
could land last, restoring stale state. `MetaFetch` in `main` now holds the
refetch to one in flight and reissues it when a write arrives mid-flight, so the
hazard is closed — but the arbitration still lives in `main`'s locals, out of
reach of the tests, which is what C moves.

### `serde_json::Value` as the inter-layer transport

`gh::fetch_files` hands back a `Value` for `model::parse_files` to walk.
`Draft::to_api() -> Value` and `Request::Review { comments: Vec<Value> }` put
wire format inside app state. `parse_meta` is 60 lines of hand-rolled
`pointer()` / `and_then` / `unwrap_or_default` that silently defaults every
field, where derived `Deserialize` would be shorter and fail loudly.

### One traversal, three implementations — *resolved (A)*

"Which threads attach to which row, in what order" is written in
`App::thread_rows`, `App::thread_ids_at_row`, and `ui::thread_rows_for_line` +
`outdated_thread_rows`. Each re-derives the outdated/resolved ranking on its
own.

### Smaller, same family

- `App::travel` writes motion arithmetic four times over, five levels deep.
  *Resolved (A): one `step(motion, current, len, viewport)` serves all four.*
- `highlights: HashMap<usize, _>` is keyed by index into `files`;
  `finish(Sent::Review(n))` assumes the submitted drafts are the first `n`.
  Positional identity, correct only until the underlying list mutates.
- `ui.rs` is four modules in one: layout, widgets, pure text measurement, and
  kitty placement math. `Theme` + `width` + `ThreadRenderState` pass by value
  into ~15 free functions, and `ThreadGroupContext` is a five-field bag rebuilt
  at each call site — a widget struct asking to exist. *Partly resolved: layout
  and measurement have moved out and both bags are gone; splitting what is left
  into `view/` is step E.*
- `App` exposes ~25 public fields and 64 public items.

## Rules

Five invariants. The tree follows from them; these are the actual contract.
Each is tagged with the step that delivers it, so a rule that is still a target
is not mistaken for one the code already keeps.

1. **The view never mutates.** *(Invariant live since A and B; the signature
   below arrives with E.)* `view::draw(&Frame, &App, &Layout) ->
   Vec<Placement>`. Anything render would otherwise discover is computed by
   layout instead. Today this is `ui::draw(&mut Frame, &App, &Layout)`, which
   already takes the model immutably; E is what renames it and lifts kitty
   placements into the return.
2. **Layout is a value, computed once per frame.** *(Live since B.)*
   `Layout::compute(area, &App)` holds the pane rects, the viewport heights,
   and the virtual row list. The loop computes it and hands the same value to
   both `apply` and `draw`. This is what removes the `viewport_height`
   parameter and the `- 3`.
3. **One effect channel.** *(Planned, C.)* `App::apply` queues an `Effect`;
   every result arrives as one `Msg`; the loop is the only thing that spawns.
   Requests carry a generation so a stale response drops instead of
   clobbering.
4. **Domain types at boundaries.** *(Planned, D.)* `serde_json::Value` never
   leaves `github/wire.rs`. `github` returns `PullRequest` /
   `Vec<ChangedFile>`; drafts serialize at the edge, never inside `app`.
5. **Grouped private state.** *(Planned, E.)* `App` owns sub-structs (`Focus`,
   `Drafts`, `Find`, `Loading`) that keep their own invariants, rather than 25
   public fields any caller can desynchronize.

## Tree

```
src/
  main.rs           args -> session -> run          (~60 lines)
  cli.rs            Args, ThemeChoice, ImageChoice
  model/            PullRequest, ChangedFile, DiffLine, ReviewThread, Draft, Anchor
  github/
    client.rs       agent, token, get/post/graphql, pagination, error detail
    api.rs          the six calls, returning model types
    wire.rs         Deserialize structs + patch parsing   <- only serde_json here
  app/
    mod.rs          App: state + apply(Action | Msg) -> Effect
    effect.rs       one Effect enum
    focus.rs        cursor, pane, focused/expanded thread
    find.rs         filter + search (one concept, two targets)
    drafts.rs  review.rs  editor.rs  mode.rs
    keys.rs    keymap.rs  command.rs  ex.rs  input.rs
  layout/
    mod.rs          Layout::compute -> rects, viewports, rows
    rows.rs         Row enum + build() -> Vec<Row>         <- the key abstraction
    measure.rs      clip / width / truncate                <- pure, unit-testable
  view/
    mod.rs          draw(&App, &Layout)                    <- read-only
    diff.rs  files.rs  thread.rs  overlay.rs  statusbar.rs
  runtime/
    loop.rs  effects.rs  terminal.rs  images.rs
```

Net effect: `app/` shrinks as logic moves to `layout/`, `ui.rs` splits about
five ways, `main.rs` loses ~400 lines, and `model.rs` splits into `model/` plus
`github/wire.rs`.

Where it stands: `layout/` exists as drawn. `ui.rs` is read-only and
row-driven but is still one file, so `view/` is the shape it splits into.
`cli.rs`, `model/`, `github/`, `runtime/`, and the `app/` split are steps C
through E.

## The virtual row model

The single abstraction the diff pane has been missing. The pane is not a list
of source lines; it is a list of rows, where a row is a code line, a thread
group heading, an elision marker, a thread summary, or one line inside an
expanded thread.

`layout/rows.rs` builds that list once per frame as **descriptors, not spans**
— cheap enough to rebuild on every keystroke, since only the visible slice is
converted to styled text by the view. It replaces all three copies of the
thread traversal, deletes the measure-then-render double pass, and lets
`diff_scroll` address rows directly instead of source lines with a
walk-backwards correction.

`thread_scroll_limit` stops being state: it is a function of the row list, so it
is read off `Layout` and the model never stores it.

`diff_scroll` now counts rows rather than source lines, which is what removes
the walk-backwards height reservation in `draw_diff`. `Rows::window` clamps the
offset when it slices, so a conversation collapsing under the cursor can never
leave the viewport short — and clamping while reading keeps it out of the model.

One honest wrinkle: `Layout::compute` runs before a keystroke is dispatched, so
an action reads the row list the *previous* frame was drawn from. That is the
same frame-behind relationship the old `viewport_height` had; the difference is
that it is now a single named value the action and the renderer share, rather
than an integer re-derived in two places.

`view::draw` returns its image placements instead of the escape sequences, so it
needs only `&App`. The loop pairs the placements with `Images::frame_commands`
between `terminal::render` and `terminal::present`, which keeps both inside one
synchronized update while leaving the render pass read-only.

## Order of work

A and B are one continuous change in practice — B falls out of A. C is
independent and can lead if the refetch race needs fixing first.

- **A. Virtual row model** (`layout/rows.rs`) — **done.** One traversal, no
  double measurement, `thread_scroll_limit` out of `App`.
- **B. Layout as a value** (`layout/mod.rs`) — **done.** `viewport_height` gone
  from every call site, and the `- 3` with it.
- **C. Unified `Effect` / `Msg` with generations.** Moves the load and error
  state machine out of `main`'s locals into `App`, where tests can reach it.
  Fixes the refetch race.
- **D. `model/` + `github/wire.rs`.** `Value` out of `app`, derived
  deserialization, loud parse failures.
- **E. Split `ui.rs`, extract `measure.rs`, collapse `travel`.** Mostly
  mechanical once A through C land.

## Testing

`tests/render.rs` and `tests/modal.rs` drive a `TestBackend` and assert on
rendered screen text. That is the right top-level oracle for a TUI and it is
what makes these refactors safe, but it is currently the only level: scroll and
layout arithmetic has no unit tests because it is not separable from drawing.
Extracting `layout/` is what makes that arithmetic addressable, so it gets
direct tests as it moves — 27 lib unit tests now, up from 10.

Two things the extraction changed in the tests themselves, both worth keeping:

- Tests used to dispatch keys with a hardcoded `viewport_height: 20` while
  rendering at a height of 27. They now compute a real `Layout` from the frame
  size they draw at, so the keystroke and the frame agree.
- `expanded_thread_movement_scrolls_without_losing_focus` used to assign
  `app.thread_scroll_limit = 4` directly. The limit is derived now, so the test
  builds a conversation long enough to actually overflow its window.
