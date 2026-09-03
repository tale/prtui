use prtui_github::{parse_files, parse_meta};
use prtui_tui::app::action::{Action, Motion};
use prtui_tui::app::draft::{Parent, Side, Sync};
use prtui_tui::app::editor::CommentEditor;
use prtui_tui::app::input::{DispatchResult, InputRouter};
use prtui_tui::app::keymap::{Keymap, Resolution};
use prtui_tui::app::link::{Errand, Link};
use prtui_tui::app::mode::Mode;
use prtui_tui::app::review::{Failure, Request, ReviewEvent, Sent};
use prtui_tui::app::search::Match;
use prtui_tui::app::{App, Card, Pane};
use prtui_tui::expand::{Reveal, STEP};
use prtui_tui::layout::Layout;
use ratatui::layout::Rect;
use std::fmt::Write;
use termina::event::{KeyCode, KeyEvent, Modifiers};

/// The frame every test renders at. Layout has to be computed from the same
/// size the last frame used, since that is what the cursor still addresses.
const FRAME: Rect = Rect {
    x: 0,
    y: 0,
    width: 120,
    height: 30,
};

fn layout_of(app: &App) -> Layout {
    Layout::compute(FRAME, app)
}

/// One key through the router, laid out the way the event loop lays it out.
/// The router is threaded in because half-typed commands — a count prefix, a
/// pending `g` — live in it across keys.
fn send(
    input: &mut InputRouter,
    app: &mut App,
    event: KeyEvent,
) -> DispatchResult {
    let layout = layout_of(app);
    input.dispatch_key(app, event, &layout)
}

fn paste(input: &mut InputRouter, app: &mut App, text: &str) -> DispatchResult {
    let layout = layout_of(app);
    input.dispatch_paste(app, text, &layout)
}

fn act(app: &mut App, action: &Action) {
    let layout = layout_of(app);
    app.apply(action, &layout);
}

fn found(app: &App) -> Vec<prtui_tui::app::search::Match> {
    app.search_matches(&layout_of(app))
}

fn summary(app: &App) -> (usize, usize) {
    app.search_summary(&layout_of(app))
}

/// The fixture's threads in wire order. The app files them by path, so a test
/// that wants one as a template reaches for the fixture rather than the app.
fn fixture_threads() -> Vec<prtui_core::ReviewThread> {
    parse_meta(include_bytes!("fixtures/meta.json"))
        .unwrap()
        .threads
}

/// Answers every draft request the way GitHub would.
///
/// Drafts are written straight through now, so a test that wants the state
/// after a save has to let the round trip finish rather than staging it.
fn settle(app: &mut App) {
    for _ in 0..8 {
        let requests = app.take_requests();
        if requests.is_empty() {
            return;
        }

        for request in requests {
            match request {
                Request::AddThread { draft, .. } => {
                    app.finish(Ok(Sent::ThreadAdded {
                        draft,
                        review: "PRR_1".into(),
                        comment: format!("PRRC_{draft}").into(),
                    }));
                }
                Request::UpdateComment { draft, .. } => {
                    app.finish(Ok(Sent::CommentUpdated(draft)));
                }
                Request::DeleteComment { draft, .. } => {
                    app.finish(Ok(Sent::CommentDeleted(draft)));
                }
                other => panic!("unexpected request {other:?}"),
            }
        }
    }

    panic!("draft requests never settled");
}

fn load() -> App {
    let mut app = App::new();
    app.set_files(parse_files(include_bytes!("fixtures/files.json")).unwrap());
    app.set_meta(parse_meta(include_bytes!("fixtures/meta.json")).unwrap());
    app.pane = Pane::Diff;

    // The first fixture file is only 8 rows; motions need room to breathe.
    app.selected_file = app
        .files
        .iter()
        .enumerate()
        .max_by_key(|(_, f)| f.lines.len())
        .map(|(i, _)| i)
        .unwrap();
    app
}

/// Row 0 of any diff is a hunk header, which is deliberately not commentable.
fn park_on_code(app: &mut App) {
    app.cursor = app.files[app.selected_file]
        .lines
        .iter()
        .position(|l| l.kind != prtui_core::LineKind::Hunk)
        .unwrap();
}

fn park_on_unresolved_thread(app: &mut App) -> prtui_core::ReviewThread {
    let thread = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
    app.selected_file = app
        .files
        .iter()
        .position(|file| file.path == thread.path)
        .unwrap();
    app.cursor = app.files[app.selected_file]
        .lines
        .iter()
        .position(|line| thread.anchors_to(line))
        .unwrap();
    thread
}

/// Swaps a synthetic patch in for the open file, for diff shapes the captured
/// fixture does not contain.
fn open_patch(app: &mut App, patch: &str) {
    let mut files = app.files.clone();
    files[app.selected_file] = file_from(patch).into();
    app.set_files(files);
}

fn file_from(patch: &str) -> prtui_core::ChangedFile {
    let page = serde_json::json!([[{
        "filename": "synthetic.go",
        "status": "modified",
        "additions": 1,
        "deletions": 2,
        "patch": patch,
    }]]);

    parse_files(&serde_json::to_vec(&page).unwrap())
        .unwrap()
        .remove(0)
}

/// Sets the verdict the open submit form carries. Reaching it by tab is a
/// separate concern, covered where the overlay's keys are.
const fn choose(app: &mut App, event: ReviewEvent) {
    app.submission
        .as_mut()
        .expect("the submit form is open")
        .event = event;
}

fn press(app: &mut App, keys: &str) {
    let input = &mut InputRouter::default();
    for c in keys.chars() {
        send(input, app, KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
    }
}

#[test]
fn count_prefix_multiplies_a_motion() {
    let mut app = load();

    press(&mut app, "5j");
    assert_eq!(app.cursor, 5);

    press(&mut app, "12j");
    assert_eq!(app.cursor, 17);

    press(&mut app, "3k");
    assert_eq!(app.cursor, 14);

    // A count never walks off the end of the file.
    press(&mut app, "9999j");
    assert_eq!(app.cursor, app.diff_len() - 1);

    // Arbitrarily long terminal input is clamped instead of overflowing.
    app.cursor = 0;
    let huge_count = format!("{}j", "9".repeat(100));
    press(&mut app, &huge_count);
    assert_eq!(app.cursor, app.diff_len() - 1);
}

/// Types a `:` line and runs it, the way a reader would.
fn ex(app: &mut App, line: &str) {
    let input = &mut InputRouter::default();
    send(
        input,
        app,
        KeyEvent::new(KeyCode::Char(':'), Modifiers::NONE),
    );
    for c in line.chars() {
        send(input, app, KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
    }
    send(input, app, KeyEvent::new(KeyCode::Enter, Modifiers::NONE));
}

/// A count belongs to the keymap rather than to `j` and `k`, so every command
/// that repeats answers to one.
#[test]
fn a_count_repeats_the_command_it_precedes() {
    for (stepwise, counted) in [("]]", "2]"), ("}}", "2}")] {
        let mut one = load();
        let mut many = load();
        one.selected_file = 0;
        many.selected_file = 0;

        press(&mut one, stepwise);
        press(&mut many, counted);

        assert_eq!(one.selected_file, many.selected_file, "{counted}");
        assert_eq!(one.cursor, many.cursor, "{counted}");
        assert_eq!(one.focused_card, many.focused_card, "{counted}");
    }
}

/// `{n}G`, `{n}gg` and `:{n}` all name the line the gutter shows, which is the
/// new side of the diff rather than the row the renderer happens to put it on.
#[test]
fn a_line_number_lands_on_the_line_the_gutter_shows() {
    let mut app = load();
    let file = app.current_file().unwrap();
    let (row, number) = file
        .lines
        .iter()
        .enumerate()
        .filter_map(|(row, line)| Some((row, line.new_line?)))
        .nth(6)
        .expect("the fixture has numbered lines");

    ex(&mut app, &number.to_string());
    assert_eq!(app.cursor, row);

    app.cursor = 0;
    press(&mut app, &format!("{number}G"));
    assert_eq!(app.cursor, row);

    app.cursor = 0;
    press(&mut app, &format!("{number}gg"));
    assert_eq!(app.cursor, row);

    // Without a count both keys still mean the ends of the file.
    press(&mut app, "G");
    assert_eq!(app.cursor, app.diff_len() - 1);
    press(&mut app, "gg");
    assert_eq!(app.cursor, 0);
}

/// A line past the end of the patch has to land somewhere sane rather than
/// being refused.
#[test]
fn a_line_number_past_the_file_lands_on_its_last_row() {
    let mut app = load();

    ex(&mut app, "99999");
    assert_eq!(app.cursor, app.diff_len() - 1);
}

/// The command line and the keys share one vocabulary, so `:` reaches every
/// command whether or not a key carries it.
#[test]
fn the_command_line_runs_the_commands_the_keys_are_bound_to() {
    let mut app = load();
    ex(&mut app, "submit");
    assert_eq!(app.mode, Mode::Submit);
    act(&mut app, &Action::CancelSubmit);

    let mut app = load();
    ex(&mut app, "w");
    assert!(app.submission.is_some());

    let mut app = load();
    ex(&mut app, "q");
    assert!(app.should_quit);

    let mut app = load();
    ex(&mut app, "nope");
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.status, "not a command: nope");
    assert!(!app.should_quit);
}

#[test]
fn the_command_line_can_be_left_without_running_anything() {
    let mut app = load();
    let mut input = InputRouter::default();

    press(&mut app, ":q");
    assert_eq!(app.mode, Mode::CommandLine);

    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
    );
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.command_line.is_none());
    assert!(!app.should_quit);
}

/// What was run before is one key away, since a `:` line is usually retyped
/// rather than composed.
#[test]
fn the_command_line_remembers_what_was_run() {
    let mut app = load();
    ex(&mut app, "12");
    ex(&mut app, "nope");

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char(':'), Modifiers::NONE),
    );
    let up = KeyEvent::new(KeyCode::Up, Modifiers::NONE);
    let down = KeyEvent::new(KeyCode::Down, Modifiers::NONE);

    send(&mut input, &mut app, up);
    assert_eq!(app.command_line.as_ref().unwrap().text(), "nope");
    send(&mut input, &mut app, up);
    assert_eq!(app.command_line.as_ref().unwrap().text(), "12");
    send(&mut input, &mut app, down);
    assert_eq!(app.command_line.as_ref().unwrap().text(), "nope");
    send(&mut input, &mut app, down);
    assert_eq!(app.command_line.as_ref().unwrap().text(), "");
}

/// The reference is a view of the command table, so a command that no key
/// carries still has to be listed under the name `:` reaches it by.
#[test]
fn the_reference_lists_every_command_and_the_keys_bound_to_it() {
    use prtui_tui::app::keymap::Reference;

    let app = load();
    let reference = app.keymap().reference();

    let entries: Vec<(&str, &str)> = reference
        .iter()
        .filter_map(|line| match line {
            Reference::Entry { keys, name, .. } => Some((*name, keys.as_str())),
            Reference::Heading(_) => None,
        })
        .collect();

    let find = |name: &str| {
        entries
            .iter()
            .find(|(command, _)| *command == name)
            .map(|(_, keys)| *keys)
    };

    // The same command reached from more than one mode lists each chord once.
    assert_eq!(find("move-down"), Some("j  <Down>"));
    assert_eq!(find("history-prev"), Some("<Up>  <C-p>"));
    assert_eq!(find("expand-file"), Some("zR"));
    assert_eq!(find("help"), Some("?"));
    // Reachable only by name, and listed anyway.
    assert_eq!(find("leave-card"), Some(""));

    assert!(reference.contains(&Reference::Heading("hidden lines")));
}

/// The one errand a key left behind, which is what the event loop would carry
/// out.
fn errand(app: &mut App) -> Errand {
    let mut errands = app.take_errands();

    assert_eq!(errands.len(), 1, "expected exactly one errand");
    errands.remove(0)
}

#[test]
fn the_overview_reads_the_description_and_the_discussion() {
    let mut app = load();
    let (cursor, scroll) = (app.cursor, app.diff_scroll);

    press(&mut app, "o");
    assert_eq!(app.mode, Mode::Overview);
    assert_eq!(app.overlay_scroll, 0);

    let layout = layout_of(&app);
    let panel = drawn_lines(&layout);
    assert!(
        panel.iter().any(|line| line.contains("Relates #8995")),
        "the description is missing: {panel:?}"
    );

    // The fixture's discussion is under the description, so it takes a scroll
    // to reach.
    press(&mut app, "G");
    assert!(app.overlay_scroll > 0);
    let panel = drawn_lines(&layout_of(&app));
    assert!(
        panel.iter().any(|line| line.contains("@malept")),
        "the discussion is missing: {panel:?}"
    );

    // Reading it must not move the cursor, the way the reference does not.
    press(&mut app, "o");
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.overlay_scroll, 0);
    assert_eq!((app.cursor, app.diff_scroll), (cursor, scroll));
    assert!(!app.should_quit);
}

/// The panel's lines, as the view would paint them.
fn drawn_lines(layout: &Layout) -> Vec<String> {
    use prtui_tui::layout::Content;

    let overlay = layout.overlay.as_ref().expect("a panel is open");
    match &overlay.content {
        Content::Keys(_) => panic!("the reference is open, not the overview"),
        Content::Prose(lines) => {
            lines.iter().map(ToString::to_string).collect()
        }
    }
}

#[test]
fn yanking_a_line_links_to_the_file_at_head() {
    let mut app = load();
    park_on_code(&mut app);

    let line = app.files[app.selected_file].lines[app.cursor]
        .new_line
        .expect("parked on a line that is at head");
    let path = app.files[app.selected_file].path.clone();

    press(&mut app, "y");

    assert_eq!(
        errand(&mut app),
        Errand::Copy(Link::Blob {
            commit: app.pr.as_ref().unwrap().head_oid.clone(),
            path,
            lines: Some((line, line)),
        })
    );
    assert_eq!(app.status, "yanked link");
}

/// A span links to the whole span, and the selection has done its job once it
/// has been copied.
#[test]
fn yanking_a_visual_span_links_every_line_and_drops_the_selection() {
    let mut app = load();
    park_on_code(&mut app);

    press(&mut app, "v2j");
    assert_eq!(app.mode, Mode::Visual);
    press(&mut app, "y");

    let Errand::Copy(Link::Blob { lines, .. }) = errand(&mut app) else {
        panic!("a yank copies");
    };
    let Some((start, end)) = lines else {
        panic!("a span names its lines");
    };
    assert_ne!(start, end, "a span names both ends");

    assert_eq!(app.mode, Mode::Normal);
    assert!(app.selection.is_none());
}

/// A conversation is addressed on the web by its own comment, not by the line
/// it hangs off.
#[test]
fn yanking_a_conversation_links_the_comment() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    let reply_target = thread.comments[0]
        .reply_target
        .clone()
        .expect("the fixture's comments carry reply targets");

    press(&mut app, "j");
    assert!(app.focused_card.is_some(), "the card takes the focus");

    press(&mut app, "y");
    assert_eq!(errand(&mut app), Errand::Copy(Link::Comment(reply_target)));
}

/// A yank is addressed at the cursor; the browser is not. Opening a blob page
/// halfway through a review is not what `gx` is for.
#[test]
fn gx_opens_the_pull_request_wherever_the_cursor_is() {
    let mut app = load();
    let pull = Errand::Open(Link::PullRequest);

    park_on_code(&mut app);
    press(&mut app, "gx");
    assert_eq!(errand(&mut app), pull);
    assert_eq!(app.status, "opening the pull request");

    park_on_unresolved_thread(&mut app);
    press(&mut app, "j");
    press(&mut app, "gx");
    assert_eq!(errand(&mut app), pull);
}

/// A panel is a wall of text, and a wall of text you cannot search is a wall.
#[test]
fn the_reference_searches_and_steps_its_hits() {
    let mut app = load();
    press(&mut app, "?");

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    assert_eq!(app.mode, Mode::Search);
    paste(&mut input, &mut app, "comment");
    send(&mut input, &mut app, KeyCode::Enter.into());

    // Accepting hands the reader back to the panel, not to the diff.
    assert_eq!(app.mode, Mode::Help);

    let hits = app.overlay_matches(&layout_of(&app));
    assert!(hits.len() > 1, "the reference names comment more than once");
    assert_eq!(app.overlay_match_row(&layout_of(&app)), Some(hits[0]));

    press(&mut app, "n");
    assert_eq!(app.overlay_match_row(&layout_of(&app)), Some(hits[1]));
    press(&mut app, "N");
    assert_eq!(app.overlay_match_row(&layout_of(&app)), Some(hits[0]));

    // Both ends wrap, the way the diff's search does.
    press(&mut app, "N");
    assert_eq!(
        app.overlay_match_row(&layout_of(&app)),
        hits.last().copied()
    );
}

/// The panel has to scroll to a hit that is past the bottom of it, or the count
/// in the bar names a line nobody can see.
#[test]
fn searching_the_overview_scrolls_the_hit_into_the_panel() {
    let mut app = load();
    press(&mut app, "o");

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    paste(&mut input, &mut app, "malept");
    send(&mut input, &mut app, KeyCode::Enter.into());

    let layout = layout_of(&app);
    let row = app
        .overlay_match_row(&layout)
        .expect("the discussion matches");
    assert!(
        row >= layout.overlay_viewport(),
        "the hit is past the first screen"
    );
    assert!(app.overlay_scroll <= row);
    assert!(row < app.overlay_scroll + layout.overlay_viewport());
    assert_eq!(app.search_summary(&layout), (1, 1));
}

/// Cancelling puts the panel back where the reader left it rather than where
/// the search wandered to.
#[test]
fn cancelling_a_panel_search_restores_the_scroll() {
    let mut app = load();
    press(&mut app, "o");
    press(&mut app, "5j");
    let scroll = app.overlay_scroll;

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    paste(&mut input, &mut app, "malept");
    send(&mut input, &mut app, KeyCode::Escape.into());

    assert_eq!(app.mode, Mode::Overview);
    assert_eq!(app.overlay_scroll, scroll);
    assert!(app.search.is_none());
}

/// A search inside a panel must not disturb the review underneath it.
#[test]
fn a_panel_search_leaves_the_diff_alone() {
    let mut app = load();
    press(&mut app, "6j");
    let (cursor, scroll, pane) = (app.cursor, app.diff_scroll, app.pane);

    press(&mut app, "?");
    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    paste(&mut input, &mut app, "comment");
    send(&mut input, &mut app, KeyCode::Enter.into());
    press(&mut app, "q");

    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(
        (app.cursor, app.diff_scroll, app.pane),
        (cursor, scroll, pane)
    );
}

#[test]
fn the_reference_opens_scrolls_and_closes() {
    let mut app = load();

    press(&mut app, "?");
    assert_eq!(app.mode, Mode::Help);
    assert_eq!(app.overlay_scroll, 0);

    press(&mut app, "5j");
    assert_eq!(app.overlay_scroll, 5);
    press(&mut app, "2k");
    assert_eq!(app.overlay_scroll, 3);
    press(&mut app, "gg");
    assert_eq!(app.overlay_scroll, 0);

    // The reference is longer than the box it is read in.
    press(&mut app, "G");
    assert!(app.overlay_scroll > 0);

    press(&mut app, "q");
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.overlay_scroll, 0);
    assert!(!app.should_quit);
}

/// Reading the reference must not move the cursor, or a reader loses their
/// place by looking a key up.
#[test]
fn the_reference_leaves_the_diff_where_it_was() {
    let mut app = load();
    press(&mut app, "6j");
    let (cursor, scroll) = (app.cursor, app.diff_scroll);

    press(&mut app, "?");
    press(&mut app, "9j");
    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
    );

    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.cursor, cursor);
    assert_eq!(app.diff_scroll, scroll);
}

#[test]
fn the_reference_answers_to_the_command_line_too() {
    let mut app = load();

    ex(&mut app, "h");
    assert_eq!(app.mode, Mode::Help);

    press(&mut app, "?");
    assert_eq!(app.mode, Mode::Normal);

    ex(&mut app, "help");
    assert_eq!(app.mode, Mode::Help);
}

#[test]
fn gg_needs_both_keys() {
    let mut app = load();
    press(&mut app, "9j");
    assert_eq!(app.cursor, 9);

    // A lone `g` is incomplete and must not move the cursor.
    let mut input = InputRouter::default();
    let key = KeyEvent::new(KeyCode::Char('g'), Modifiers::NONE);
    assert_eq!(send(&mut input, &mut app, key), DispatchResult::Pending);
    assert_eq!(app.cursor, 9);
    assert_eq!(app.pending_hint(), "g");

    assert_eq!(
        send(&mut input, &mut app, key),
        DispatchResult::Applied(Action::Move(Motion::Top))
    );
    assert_eq!(app.cursor, 0);
    assert!(app.pending_hint().is_empty());
}

/// Hidden lines answer to Vim's fold keys, and a count on one says how many
/// lines to pull in rather than how many times to repeat the command.
#[test]
fn z_opens_hidden_lines_the_way_it_opens_folds() {
    fn chord(keymap: &mut Keymap, keys: &str) -> Resolution {
        let mut last = Resolution::Unbound;
        for c in keys.chars() {
            last = keymap.resolve(
                Mode::Normal,
                KeyEvent::new(KeyCode::Char(c), Modifiers::NONE),
            );
        }

        last
    }

    let keymap = &mut Keymap::default();

    assert_eq!(
        chord(keymap, "zk"),
        Resolution::Action(Action::Expand(Reveal::Up(STEP)))
    );
    assert_eq!(
        chord(keymap, "zj"),
        Resolution::Action(Action::Expand(Reveal::Down(STEP)))
    );
    assert_eq!(
        chord(keymap, "za"),
        Resolution::Action(Action::Expand(Reveal::All))
    );
    assert_eq!(chord(keymap, "zR"), Resolution::Action(Action::ExpandFile));

    // A count is lines, not repetitions.
    assert_eq!(
        chord(keymap, "120zk"),
        Resolution::Action(Action::Expand(Reveal::Up(120)))
    );

    // A lone `z` waits for its second key, and an unbound one drops the chord.
    assert_eq!(
        keymap.resolve(
            Mode::Normal,
            KeyEvent::new(KeyCode::Char('z'), Modifiers::NONE)
        ),
        Resolution::Pending
    );
    assert_eq!(keymap.pending_hint(), "z");
    assert_eq!(chord(keymap, "q"), Resolution::Unbound);
    assert!(keymap.pending_hint().is_empty());
}

/// The fold keys act on the diff, where the cursor names a run of hidden
/// lines. Visual mode has a selection to keep, so `z` is not its key.
#[test]
fn z_is_not_bound_outside_normal_mode() {
    let mut keymap = Keymap::default();
    let key = KeyEvent::new(KeyCode::Char('z'), Modifiers::NONE);

    assert_eq!(keymap.resolve(Mode::Visual, key), Resolution::Unbound);
}

#[test]
fn leading_zero_is_unbound_and_does_not_start_a_count() {
    let mut app = load();
    let mut keymap = Keymap::default();
    let key = KeyEvent::new(KeyCode::Char('0'), Modifiers::NONE);

    // Leading zero must not start a count that swallows the next motion.
    assert_eq!(keymap.resolve(Mode::Normal, key), Resolution::Unbound);
    press(&mut app, "j");
    assert_eq!(app.cursor, 1);
}

#[test]
fn visual_selection_grows_from_its_anchor() {
    let mut app = load();

    press(&mut app, "3j");
    press(&mut app, "V");
    assert_eq!(app.mode, Mode::Visual);

    press(&mut app, "4j");
    let selection = app.selection.unwrap();
    assert_eq!(selection.anchor, 3);
    assert_eq!(selection.head, 7);
    assert_eq!(selection.row_count(), 5);

    // Extending upward past the anchor keeps the range inclusive and ordered.
    press(&mut app, "6k");
    let selection = app.selection.unwrap();
    assert_eq!(*selection.range().start(), 1);
    assert_eq!(*selection.range().end(), 3);
}

#[test]
fn normal_movement_visits_threads_between_source_lines() {
    let mut app = load();
    let first = park_on_unresolved_thread(&mut app);
    let mut second = first.clone();
    second.id = "second-thread".into();
    app.threads_by_path
        .insert(first.path.clone(), vec![first.clone(), second.clone()]);
    let anchor = app.cursor;

    press(&mut app, "j");
    assert_eq!(app.cursor, anchor);
    assert_eq!(app.focused_thread(), Some(&*first.id));

    press(&mut app, "j");
    assert_eq!(app.cursor, anchor);
    assert_eq!(app.focused_thread(), Some(&*second.id));

    press(&mut app, "j");
    assert_eq!(app.cursor, anchor + 1);
    assert!(app.focused_card.is_none());

    press(&mut app, "k");
    assert_eq!(app.cursor, anchor);
    assert_eq!(app.focused_thread(), Some(&*second.id));
}

#[test]
fn enter_toggles_the_focused_thread() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    press(&mut app, "j");
    assert_eq!(app.focused_thread(), Some(&*thread.id));

    let mut input = InputRouter::default();
    send(&mut input, &mut app, KeyCode::Enter.into());
    assert_eq!(
        app.expanded_card
            .as_ref()
            .and_then(Card::thread)
            .map(|id| &**id),
        Some(&*thread.id)
    );

    send(&mut input, &mut app, KeyCode::Enter.into());
    assert!(app.expanded_card.is_none());
}

#[test]
fn expanded_thread_movement_scrolls_without_losing_focus() {
    let mut app = load();
    let mut thread = park_on_unresolved_thread(&mut app);

    // Long enough that the conversation genuinely overflows its window, which
    // is what gives the motion something to scroll.
    thread.comments[0].body =
        (1..=40).fold(String::new(), |mut body, index| {
            let _ = writeln!(body, "line {index}\n");
            body
        });
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);

    press(&mut app, "j");
    let mut input = InputRouter::default();
    send(&mut input, &mut app, KeyCode::Enter.into());
    assert!(app.expanded_card.is_some());
    assert!(layout_of(&app).rows.body_limit() > 4);

    press(&mut app, "j");
    assert_eq!(app.thread_scroll, 1);
    assert_eq!(app.focused_thread(), Some(&*thread.id));
    assert_eq!(
        app.expanded_card
            .as_ref()
            .and_then(Card::thread)
            .map(|id| &**id),
        Some(&*thread.id)
    );

    press(&mut app, "k");
    assert_eq!(app.thread_scroll, 0);
}

#[test]
fn escape_returns_from_a_thread_to_its_source_line() {
    let mut app = load();
    park_on_unresolved_thread(&mut app);
    press(&mut app, "j");
    let source_row = app.cursor;

    let mut input = InputRouter::default();
    assert_eq!(
        send(&mut input, &mut app, KeyCode::Escape.into()),
        DispatchResult::Applied(Action::Escape)
    );
    assert_eq!(app.cursor, source_row);
    assert!(app.focused_card.is_none());
    assert!(!app.should_quit);
}

#[test]
fn visual_movement_remains_source_line_only() {
    let mut app = load();
    park_on_unresolved_thread(&mut app);
    let anchor = app.cursor;

    press(&mut app, "Vj");

    assert_eq!(app.cursor, anchor + 1);
    assert!(app.focused_card.is_none());
}

#[test]
fn leaving_visual_clears_the_selection() {
    let mut app = load();
    press(&mut app, "V");
    assert!(app.selection.is_some());

    press(&mut app, "V");
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.selection.is_none());
}

#[test]
fn commenting_a_selection_produces_one_multiline_draft() {
    let mut app = load();

    // Park on a real added line so the anchor resolves to the new side.
    let added = app.files[app.selected_file]
        .lines
        .iter()
        .position(|l| l.kind == prtui_core::LineKind::Added)
        .unwrap();
    app.cursor = added;

    press(&mut app, "V");
    press(&mut app, "2j");
    press(&mut app, "c");
    assert_eq!(app.mode, Mode::Insert);
    assert!(app.composer.is_some());

    let composer = app.composer.as_mut().unwrap();
    composer.editor.set_text("this allocates on every call");

    act(&mut app, &Action::CommitComment);

    assert_eq!(app.mode, Mode::Normal);
    assert!(app.selection.is_none());
    assert_eq!(app.drafts.len(), 1);

    let draft = &app.drafts[0];
    assert_eq!(draft.anchor().unwrap().side, Side::Right);
    assert_eq!(draft.body, "this allocates on every call");
    assert!(draft.anchor().unwrap().is_multiline());
    assert!(
        draft.anchor().unwrap().start_line < draft.anchor().unwrap().end_line
    );
    assert_eq!(
        *draft.rows().unwrap(),
        added..=added + 2,
        "the whole block is covered"
    );
}

/// A selection running from deletions into additions used to collapse onto the
/// one line that had a new-file number; it spans both sides now.
#[test]
fn a_selection_across_both_sides_keeps_the_whole_block() {
    let mut app = load();

    open_patch(
        &mut app,
        "@@ -1,4 +1,4 @@\n context\n-gone one\n-gone two\n+added one\n context after",
    );
    app.cursor = 1;

    // context, -gone one, -gone two, +added one
    press(&mut app, "V3j");
    press(&mut app, "c");
    app.composer
        .as_mut()
        .unwrap()
        .editor
        .set_text("rewrite this");
    act(&mut app, &Action::CommitComment);

    let anchor = *app.drafts[0].anchor().unwrap();
    assert_eq!(anchor.start_side, Side::Left, "starts on the deletions");
    assert_eq!(anchor.start_line, 2, "the first deleted line");
    assert_eq!(anchor.side, Side::Right, "ends on the addition");
    assert_eq!(anchor.end_line, 2, "the added line");
    assert!(anchor.is_multiline(), "a cross-side span is never one line");
}

/// Ending back on a deletion has no cross-side form, so it stays on the left
/// rather than pairing a right-hand start with a left-hand end.
#[test]
fn a_selection_ending_in_deletions_stays_on_the_old_side() {
    let mut app = load();

    open_patch(
        &mut app,
        "@@ -1,3 +1,3 @@\n context\n-gone one\n-gone two\n",
    );
    app.cursor = 1;

    press(&mut app, "V2j");
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("drop these");
    act(&mut app, &Action::CommitComment);

    let anchor = *app.drafts[0].anchor().unwrap();
    assert_eq!(anchor.start_side, Side::Left);
    assert_eq!(anchor.side, Side::Left);
    assert_eq!((anchor.start_line, anchor.end_line), (2, 3));
}

#[test]
fn an_empty_comment_is_discarded() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    assert_eq!(app.mode, Mode::Insert);

    act(&mut app, &Action::CommitComment);
    assert!(app.drafts.is_empty());
    assert_eq!(app.mode, Mode::Normal);
}

/// Work is not thrown away on one key: the first escape arms and says so, and
/// the second is what discards.
#[test]
fn cancelling_keeps_the_buffer_out_of_the_drafts() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");

    let composer = app.composer.as_mut().unwrap();
    composer.editor.set_text("never mind");

    let mut input = InputRouter::default();
    let cancel = KeyEvent::new(KeyCode::Escape, Modifiers::NONE);
    assert_eq!(
        send(&mut input, &mut app, cancel),
        DispatchResult::Applied(Action::CancelComment)
    );
    assert!(app.composer.is_some(), "the first escape only warns");
    assert_eq!(app.status, "esc again to discard");
    assert_eq!(app.mode, Mode::Insert);

    send(&mut input, &mut app, cancel);
    assert!(app.composer.is_none());
    assert!(app.drafts.is_empty());
    assert_eq!(app.mode, Mode::Normal);
}

/// Anything but a second escape stands the composer back down, so a stray key
/// cannot leave it one press from losing the body.
#[test]
fn typing_after_an_escape_disarms_the_discard() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("keep me");

    let mut input = InputRouter::default();
    let cancel = KeyEvent::new(KeyCode::Escape, Modifiers::NONE);
    send(&mut input, &mut app, cancel);
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('x'), Modifiers::NONE),
    );
    assert!(!app.composer.as_ref().unwrap().is_discard_armed);

    send(&mut input, &mut app, cancel);
    assert!(app.composer.is_some(), "the count started over");
}

/// A reopened draft that was not touched closes on one key: there is nothing to
/// lose.
#[test]
fn escaping_an_unchanged_composer_closes_it_at_once() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("saved");
    act(&mut app, &Action::CommitComment);

    press(&mut app, "e");
    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
    );

    assert!(app.composer.is_none());
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.drafts.len(), 1, "the draft is untouched");
}

#[test]
fn insert_mode_reserves_app_chords_and_forwards_editor_keys() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");

    let mut input = InputRouter::default();

    // A bare letter belongs to the editor widget, not the app keymap.
    let letter = KeyEvent::new(KeyCode::Char('s'), Modifiers::NONE);
    assert_eq!(
        send(&mut input, &mut app, letter),
        DispatchResult::ForwardedToEditor
    );
    assert_eq!(app.composer.as_ref().unwrap().editor.text(), "s");

    // Extra modifiers do not accidentally trigger an application chord.
    let modified =
        KeyEvent::new(KeyCode::Char('s'), Modifiers::CONTROL | Modifiers::ALT);
    assert_eq!(
        send(&mut input, &mut app, modified),
        DispatchResult::ForwardedToEditor
    );
    assert!(app.composer.is_some());

    // Shifted Enter is the newline; the bare one saves.
    let newline = KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT);
    assert_eq!(
        send(&mut input, &mut app, newline),
        DispatchResult::ForwardedToEditor
    );
    assert_eq!(app.composer.as_ref().unwrap().editor.text(), "s\n");

    assert_eq!(
        send(&mut input, &mut app, KeyEvent::from(KeyCode::Enter)),
        DispatchResult::Applied(Action::CommitComment)
    );
    assert!(app.composer.is_none());
    assert_eq!(app.drafts[0].body, "s", "the trailing newline is trimmed");
}

#[test]
fn ctrl_s_no_longer_commits_anything() {
    let chord = KeyEvent::new(KeyCode::Char('s'), Modifiers::CONTROL);

    for mode in [Mode::Insert, Mode::Submit] {
        let mut app = load();
        match mode {
            Mode::Insert => {
                park_on_code(&mut app);
                press(&mut app, "c");
            }
            _ => press(&mut app, "s"),
        }
        assert_eq!(app.mode, mode);

        let mut input = InputRouter::default();
        send(&mut input, &mut app, chord);
        assert_eq!(app.mode, mode, "ctrl-s is inert in {mode:?}");
        assert!(app.drafts.is_empty());
        assert!(app.take_requests().is_empty());
    }
}

const fn ctrl(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), Modifiers::CONTROL)
}

const fn alt(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), Modifiers::ALT)
}

fn type_line(input: &mut InputRouter, app: &mut App, text: &str) {
    for character in text.chars() {
        send(
            input,
            app,
            KeyEvent::new(KeyCode::Char(character), Modifiers::NONE),
        );
    }
}

/// A prompt is a line of text in a terminal, so it answers to the chords a
/// terminal edits a line with rather than to the arrow keys alone.
#[test]
fn a_prompt_answers_to_the_readline_chords() {
    let mut app = load();
    let mut input = InputRouter::default();

    send(&mut input, &mut app, KeyEvent::from(KeyCode::Char(':')));
    type_line(&mut input, &mut app, "open src/app/main.rs");

    // Ctrl+W takes the whole path, the way the shell it came from does.
    send(&mut input, &mut app, ctrl('w'));
    assert_eq!(app.command_line.as_ref().unwrap().text(), "open ");

    type_line(&mut input, &mut app, "src/app/main.rs");
    send(&mut input, &mut app, alt('b'));
    send(&mut input, &mut app, ctrl('k'));
    assert_eq!(
        app.command_line.as_ref().unwrap().text(),
        "open src/app/main."
    );

    send(&mut input, &mut app, ctrl('a'));
    type_line(&mut input, &mut app, "x");
    assert_eq!(
        app.command_line.as_ref().unwrap().text(),
        "xopen src/app/main."
    );

    send(&mut input, &mut app, ctrl('e'));
    send(&mut input, &mut app, ctrl('u'));
    assert_eq!(app.command_line.as_ref().unwrap().text(), "");
}

/// The composer is the one prompt whose keys otherwise belong to the editor
/// widget, and it takes the same chords.
#[test]
fn the_composer_answers_to_them_too() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");

    let mut input = InputRouter::default();
    type_line(&mut input, &mut app, "one two");
    send(&mut input, &mut app, ctrl('a'));
    type_line(&mut input, &mut app, "> ");

    assert_eq!(app.composer.as_ref().unwrap().editor.text(), "> one two");
}

/// Moving around inside a recalled line is not typing over it, so the next
/// recall goes on back through the history rather than starting again.
#[test]
fn a_motion_keeps_the_place_in_the_history() {
    let mut app = load();
    ex(&mut app, "12");
    ex(&mut app, "nope");

    let mut input = InputRouter::default();
    send(&mut input, &mut app, KeyEvent::from(KeyCode::Char(':')));

    send(&mut input, &mut app, ctrl('p'));
    assert_eq!(app.command_line.as_ref().unwrap().text(), "nope");

    send(&mut input, &mut app, ctrl('a'));
    send(&mut input, &mut app, ctrl('p'));
    assert_eq!(app.command_line.as_ref().unwrap().text(), "12");
}

#[test]
fn paste_is_routed_only_to_an_open_composer() {
    let mut app = load();
    let mut input = InputRouter::default();

    assert_eq!(
        paste(&mut input, &mut app, "ignored"),
        DispatchResult::Ignored
    );

    park_on_code(&mut app);
    press(&mut app, "c");
    assert_eq!(
        paste(&mut input, &mut app, "pasted text"),
        DispatchResult::ForwardedToEditor
    );
    assert_eq!(app.composer.as_ref().unwrap().editor.text(), "pasted text");
}

#[test]
fn alt_modified_normal_bindings_are_ignored() {
    let mut app = load();
    let mut input = InputRouter::default();
    let alt_j = KeyEvent::new(KeyCode::Char('j'), Modifiers::ALT);

    assert_eq!(send(&mut input, &mut app, alt_j), DispatchResult::Ignored);
    assert_eq!(app.cursor, 0);
}

#[test]
fn a_hunk_header_is_not_commentable() {
    let mut app = load();
    app.cursor = 0;

    press(&mut app, "c");

    assert_eq!(app.mode, Mode::Normal);
    assert!(app.composer.is_none());
    assert!(app.status.contains("cannot comment"));
}

/// GitHub anchors a span inside one hunk and rejects the whole review over one
/// that is not, so the selection has to be refused before anything is typed.
#[test]
fn a_selection_across_a_hunk_header_is_not_commentable() {
    let mut app = load();
    open_patch(
        &mut app,
        "@@ -1,2 +1,2 @@\n context\n+first hunk\n@@ -20,2 +20,2 @@\n more context\n+second hunk",
    );
    app.cursor = 1;

    // context, +first hunk, @@ header, more context
    press(&mut app, "V3j");
    press(&mut app, "c");

    assert!(app.composer.is_none());
    assert!(app.status.contains("cannot comment"), "{}", app.status);
    // The selection survives the refusal, so it can be shrunk and retried.
    assert_eq!(app.mode, Mode::Visual);

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
    );
    app.cursor = 1;
    press(&mut app, "V1j");
    press(&mut app, "c");

    assert_eq!(app.mode, Mode::Insert, "one hunk still takes a comment");
}

#[test]
fn escape_and_ctrl_bracket_both_cancel_the_composer() {
    // The Kitty protocol reports these as distinct events, so each is bound
    // separately and both must reach the same action.
    let escape = KeyEvent::new(KeyCode::Escape, Modifiers::NONE);
    let ctrl_bracket = KeyEvent::new(KeyCode::Char('['), Modifiers::CONTROL);

    for key in [escape, ctrl_bracket] {
        let mut app = load();
        park_on_code(&mut app);
        press(&mut app, "c");

        app.composer
            .as_mut()
            .unwrap()
            .editor
            .set_text("half-written");
        assert_eq!(app.mode, Mode::Insert);

        let mut input = InputRouter::default();
        send(&mut input, &mut app, key);
        send(&mut input, &mut app, key);

        assert_eq!(app.mode, Mode::Normal, "{key:?} should leave insert mode");
        assert!(app.composer.is_none(), "{key:?} should close the composer");
        assert!(app.drafts.is_empty(), "{key:?} must not save the draft");
    }
}

#[test]
fn ctrl_c_quits_from_every_mode() {
    let ctrl_c = KeyEvent::new(KeyCode::Char('c'), Modifiers::CONTROL);

    for mode in [
        Mode::Normal,
        Mode::Visual,
        Mode::Insert,
        Mode::Filter,
        Mode::Search,
        Mode::CommandLine,
        Mode::Help,
        Mode::Overview,
        Mode::Submit,
    ] {
        let mut app = load();
        match mode {
            Mode::Normal => {}
            Mode::Visual => press(&mut app, "V"),
            Mode::Insert => {
                park_on_code(&mut app);
                press(&mut app, "c");
            }
            Mode::Filter => {
                app.pane = Pane::Files;
                press(&mut app, "/");
            }
            Mode::Search => press(&mut app, "/"),
            Mode::CommandLine => press(&mut app, ":"),
            Mode::Help => press(&mut app, "?"),
            Mode::Overview => press(&mut app, "o"),
            Mode::Submit => press(&mut app, "s"),
        }
        assert_eq!(app.mode, mode);

        let mut input = InputRouter::default();
        assert_eq!(
            send(&mut input, &mut app, ctrl_c),
            DispatchResult::Applied(Action::Quit)
        );
        assert!(app.should_quit, "Ctrl+C should quit from {mode:?}");
    }
}

#[test]
/// `<Esc>` used to quit outright, which is the one thing nobody means by it:
/// it is the key you press to get out of a state you did not want to be in.
fn escape_says_how_to_quit_rather_than_quitting() {
    for key in [
        KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
        KeyEvent::new(KeyCode::Char('['), Modifiers::CONTROL),
    ] {
        let mut app = load();
        let mut input = InputRouter::default();
        assert_eq!(
            send(&mut input, &mut app, key),
            DispatchResult::Applied(Action::Escape)
        );
        assert!(!app.should_quit);
        assert_eq!(app.status, "press q to quit");
    }

    let mut app = load();
    press(&mut app, "q");
    assert!(app.should_quit);
}

#[test]
fn ctrl_bracket_leaves_visual_mode() {
    let mut app = load();
    press(&mut app, "V");
    assert_eq!(app.mode, Mode::Visual);

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('['), Modifiers::CONTROL),
    );

    assert_eq!(app.mode, Mode::Normal);
    assert!(app.selection.is_none());
}

#[test]
fn a_bare_bracket_still_navigates_files() {
    let mut app = load();
    app.selected_file = 2;

    press(&mut app, "q");
    assert!(app.should_quit, "q quits from normal mode");
    app.should_quit = false;

    press(&mut app, "[");
    assert_eq!(app.selected_file, 1, "unmodified [ is still prev-file");

    press(&mut app, "]");
    assert_eq!(app.selected_file, 2, "unmodified ] is still next-file");
}

#[test]
fn pane_focus_has_tab_directional_and_enter_routes() {
    let mut app = load();
    assert_eq!(app.pane, Pane::Diff);

    press(&mut app, "h");
    assert_eq!(app.pane, Pane::Files);
    press(&mut app, "l");
    assert_eq!(app.pane, Pane::Diff);

    app.is_files_visible = false;
    let mut input = InputRouter::default();
    send(&mut input, &mut app, KeyCode::Tab.into());
    assert!(app.is_files_visible);
    assert_eq!(app.pane, Pane::Files);

    send(&mut input, &mut app, KeyCode::Enter.into());
    assert_eq!(app.pane, Pane::Diff);

    send(&mut input, &mut app, KeyCode::Left.into());
    assert_eq!(app.pane, Pane::Files);
    send(&mut input, &mut app, KeyCode::Right.into());
    assert_eq!(app.pane, Pane::Diff);
}

#[test]
fn filtering_narrows_the_tree_and_survives_commit() {
    let mut app = load();
    app.pane = Pane::Files;
    let mut input = InputRouter::default();

    press(&mut app, "/auth_check");
    assert_eq!(app.mode, Mode::Filter);
    assert_eq!(app.filtered_file_indices(), vec![2, 3]);

    send(&mut input, &mut app, KeyEvent::from(KeyCode::Up));
    assert_eq!(app.selected_file, 2, "up steps back through matches");

    send(&mut input, &mut app, KeyCode::Enter.into());
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.filter_query().as_deref(), Some("auth_check"));

    // Both matches share one directory, which names the pane rather than taking
    // a row, so the filtered tree is a flat list of the two.
    for (keys, selected) in [("j", 3), ("gg", 2), ("G", 3)] {
        press(&mut app, keys);
        assert_eq!(
            app.selected_file, selected,
            "{keys} stays within the matches"
        );
        assert_eq!(app.tree_directory(), None, "{keys} lands on a file");
    }

    send(&mut input, &mut app, KeyCode::Enter.into());
    assert_eq!(app.pane, Pane::Diff, "enter opens the selected match");
}

#[test]
fn escape_puts_the_cursor_back_where_the_filter_found_it() {
    let mut app = load();
    app.pane = Pane::Files;
    let original_file = app.selected_file;
    let mut input = InputRouter::default();

    press(&mut app, "/");
    paste(&mut input, &mut app, "nothing\nwill\rmatch");
    assert!(app.filtered_file_indices().is_empty());

    send(&mut input, &mut app, KeyCode::Enter.into());
    assert_eq!(
        app.mode,
        Mode::Filter,
        "enter will not commit an empty result"
    );

    send(&mut input, &mut app, KeyCode::Escape.into());
    assert!(app.file_filter.is_none());
    assert_eq!(app.selected_file, original_file);

    // `/` opens on the whole tree rather than onto the committed query, so a
    // second one types a new filter instead of extending the old one.
    press(&mut app, "/auth_check");
    send(&mut input, &mut app, KeyCode::Enter.into());
    app.selected_file = 2;

    press(&mut app, "/");
    assert_eq!(app.filter_query().as_deref(), Some(""), "`/` opens clean");
    assert_eq!(
        app.filtered_file_indices().len(),
        app.files.len(),
        "and on every file"
    );

    press(&mut app, "_test");
    assert_eq!(app.filter_query().as_deref(), Some("_test"));

    send(&mut input, &mut app, KeyCode::Escape.into());
    assert!(app.file_filter.is_none(), "and leaves none behind");
    assert_eq!(app.selected_file, 2, "but the cursor goes back");
}

#[test]
fn escape_clears_a_committed_filter_first() {
    for clear in [
        KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
        KeyEvent::new(KeyCode::Char('['), Modifiers::CONTROL),
    ] {
        let mut app = load();
        app.pane = Pane::Files;
        let mut input = InputRouter::default();
        send(
            &mut input,
            &mut app,
            KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
        );
        paste(&mut input, &mut app, "auth_check");
        send(&mut input, &mut app, KeyCode::Enter.into());

        // One key, one action: which of the three it did is a fact about the
        // app, so the state is what says so.
        assert_eq!(
            send(&mut input, &mut app, clear),
            DispatchResult::Applied(Action::Escape)
        );
        assert!(app.file_filter.is_none());
        assert!(!app.should_quit);

        send(&mut input, &mut app, clear);
        assert!(!app.should_quit);
        assert_eq!(app.status, "press q to quit");
    }
}

#[test]
fn comment_jump_crosses_files_and_skips_resolved_threads() {
    let mut app = load();
    app.selected_file = 0;
    app.cursor = 0;
    app.pane = Pane::Files;

    let unresolved = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();

    press(&mut app, "}");

    assert_eq!(app.pane, Pane::Diff);
    assert_eq!(app.files[app.selected_file].path, unresolved.path);
    assert_eq!(app.focused_thread(), Some(&*unresolved.id));
    assert!(
        unresolved.anchors_to(&app.files[app.selected_file].lines[app.cursor])
    );
}

/// The conversations are a ring. Walking off the last one comes back to the
/// first, so a review read from the middle never hides what is above it.
#[test]
fn comment_jump_wraps_round_to_the_first() {
    let mut app = load();
    app.selected_file = 0;
    app.cursor = 0;

    press(&mut app, "}");
    let first = (app.selected_file, app.focused_card.clone());

    let wrapped = (0..20).any(|_| {
        press(&mut app, "}");
        (app.selected_file, app.focused_card.clone()) == first
    });

    assert!(wrapped, "walking on comes back to the first conversation");
    assert_eq!(app.status, "wrapped to the top");
}

/// With nothing anywhere to jump to, the key still has to say why the reader
/// did not move.
#[test]
fn comment_jump_reports_when_there_are_none() {
    let mut app = App::new();
    app.set_files(parse_files(include_bytes!("fixtures/files.json")).unwrap());
    app.pane = Pane::Diff;

    press(&mut app, "}");

    assert_eq!(app.status, "no more comments");
}

/// `]` and `[` treat the tree as a ring for the same reason `}` does.
#[test]
fn stepping_past_either_end_of_the_tree_comes_round() {
    let mut app = load();
    let last = layout_of(&app).files.files().count() - 1;
    let order: Vec<usize> = layout_of(&app).files.files().collect();

    app.selected_file = order[last];
    press(&mut app, "]");
    assert_eq!(app.selected_file, order[0]);
    assert_eq!(app.status, "wrapped to the top");

    press(&mut app, "[");
    assert_eq!(app.selected_file, order[last]);
    assert_eq!(app.status, "wrapped to the bottom");

    // A count that laps the tree lands where a bare step would.
    app.selected_file = order[0];
    press(&mut app, &format!("{}]", order.len() + 1));
    assert_eq!(app.selected_file, order[1]);
}

#[test]
fn comment_jump_steps_through_every_thread_in_a_file() {
    let mut app = load();
    let file = app.files[app.selected_file].clone();

    let rows: Vec<usize> = file
        .lines
        .iter()
        .enumerate()
        .filter(|(_, line)| line.new_line.is_some())
        .map(|(row, _)| row)
        .take(3)
        .collect();

    let template = fixture_threads().remove(0);
    let threads: Vec<prtui_core::ReviewThread> = rows
        .iter()
        .enumerate()
        .map(|(index, &row)| prtui_core::ReviewThread {
            id: format!("thread-{index}").into(),
            path: file.path.clone(),
            line: file.lines[row].new_line,
            original_line: None,
            is_resolved: false,
            is_outdated: false,
            ..template.clone()
        })
        .collect();
    app.threads_by_path.insert(file.path.clone(), threads);

    app.cursor = 0;
    app.focused_card = None;

    for &row in &rows {
        press(&mut app, "}");
        assert_eq!(app.cursor, row);
    }
    assert_eq!(app.focused_thread(), Some("thread-2"));

    for &row in rows.iter().rev().skip(1) {
        press(&mut app, "{");
        assert_eq!(app.cursor, row);
    }
    assert_eq!(app.focused_thread(), Some("thread-0"));
}

fn search_for(app: &mut App, query: &str) -> InputRouter {
    let mut input = InputRouter::default();

    app.pane = Pane::Diff;
    send(
        &mut input,
        app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    paste(&mut input, app, query);

    input
}

#[test]
fn slash_filters_the_tree_from_the_files_pane_and_searches_from_the_diff() {
    let mut app = load();
    let mut input = InputRouter::default();

    app.pane = Pane::Files;
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    assert_eq!(app.mode, Mode::Filter);
    send(&mut input, &mut app, KeyCode::Escape.into());

    app.pane = Pane::Diff;
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    assert_eq!(app.mode, Mode::Search);
    assert!(app.file_filter.is_none());
}

#[test]
fn n_cycles_every_match_and_wraps_at_the_end() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    send(&mut input, &mut app, KeyCode::Enter.into());

    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(summary(&app).0, 1, "accepting lands on the first match");

    let total = found(&app).len();
    assert!(total > 1, "a one-match needle cannot exercise wrapping");

    let mut visited = vec![app.cursor];
    for _ in 1..total {
        press(&mut app, "n");
        visited.push(app.cursor);
    }
    assert_eq!(summary(&app).0, total, "n reaches the last match");

    let mut distinct = visited.clone();
    distinct.dedup();
    assert_eq!(distinct, visited, "each press advances to a new row");

    press(&mut app, "n");
    assert_eq!(app.cursor, visited[0], "the last match wraps to the first");
    assert_eq!(summary(&app).0, 1);

    press(&mut app, "N");
    assert_eq!(
        app.cursor,
        *visited.last().unwrap(),
        "N off the front wraps to the last match"
    );

    for expected in visited.iter().rev().skip(1) {
        press(&mut app, "N");
        assert_eq!(app.cursor, *expected, "N retraces the list in reverse");
    }
    assert_eq!(summary(&app).0, 1);
}

#[test]
fn search_matches_comment_bodies_and_focuses_the_thread() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    app.cursor = 0;
    app.focused_card = None;

    let word = thread.comments[0]
        .body
        .split_whitespace()
        .find(|word| word.len() > 5 && word.chars().all(char::is_alphanumeric))
        .expect("fixture comment has a searchable word")
        .to_string();

    let mut input = search_for(&mut app, &word);
    send(&mut input, &mut app, KeyCode::Enter.into());

    let matches = found(&app);
    assert!(
        matches
            .iter()
            .any(|hit| hit.card() == Some(Card::Thread(thread.id.clone()))),
        "the comment body should match"
    );

    for _ in 0..matches.len() {
        if app.focused_thread() == Some(&*thread.id) {
            break;
        }
        press(&mut app, "n");
    }

    assert_eq!(app.focused_thread(), Some(&*thread.id));
    assert!(thread.anchors_to(&app.files[app.selected_file].lines[app.cursor]));
}

#[test]
fn escape_restores_the_diff_position_the_search_previewed_away_from() {
    let mut app = load();
    app.cursor = 3;
    app.diff_scroll = 1;
    let needle = app.files[app.selected_file].lines[9]
        .text
        .trim()
        .to_string();

    let mut input = search_for(&mut app, &needle);
    assert_eq!(app.cursor, 9, "typing previews the first match");

    send(&mut input, &mut app, KeyCode::Escape.into());

    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.cursor, 3);
    assert_eq!(app.diff_scroll, 1);
    assert!(app.search.is_none());
}

/// The two boxes used to disagree: the diff was smartcase and the tree was
/// unconditionally case-insensitive. One matcher now serves both.
#[test]
fn the_tree_filter_reads_case_the_way_the_diff_search_does() {
    let mut app = load();

    app.is_files_visible = true;
    app.pane = Pane::Files;

    press(&mut app, "/verify");
    assert_eq!(
        app.filtered_file_indices().len(),
        2,
        "lowercase ignores case"
    );

    act(&mut app, &Action::CancelFileFilter);
    press(&mut app, "/Verify");
    assert!(
        app.filtered_file_indices().is_empty(),
        "a capital makes the query exact, and no path spells it that way"
    );
}

#[test]
fn escape_clears_a_committed_search_before_anything_else() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    send(&mut input, &mut app, KeyCode::Enter.into());
    assert!(app.search.is_some());

    send(&mut input, &mut app, KeyCode::Escape.into());
    assert!(app.search.is_none());
    assert!(!app.should_quit, "the first escape only clears the pattern");

    send(&mut input, &mut app, KeyCode::Escape.into());
    assert!(!app.should_quit);
    assert_eq!(app.status, "press q to quit");
}

#[test]
fn match_and_comment_motions_are_normal_mode_only() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    send(&mut input, &mut app, KeyCode::Enter.into());

    press(&mut app, "V");
    let anchored = app.cursor;

    for key in ["n", "N", "}", "{"] {
        press(&mut app, key);
        assert_eq!(
            app.cursor, anchored,
            "{key} must not move the cursor in visual"
        );
        assert_eq!(app.mode, Mode::Visual, "{key} must not leave visual");
    }
}

#[test]
fn ctrl_d_and_ctrl_u_step_by_half_a_viewport() {
    let mut app = load();
    app.pane = Pane::Diff;
    let mut input = InputRouter::default();
    let half = layout_of(&app).diff_viewport() / 2;

    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), Modifiers::CONTROL),
    );
    assert!(
        (1..=half).contains(&app.cursor),
        "ctrl-d advances at most half a viewport, landed on {}",
        app.cursor
    );

    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL),
    );
    assert_eq!(app.cursor, 0, "ctrl-u walks the same distance back");
}

#[test]
fn match_motions_find_the_nearest_hit_from_an_unmatched_row() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    send(&mut input, &mut app, KeyCode::Enter.into());

    let rows: Vec<usize> = found(&app).iter().map(Match::row).collect();
    let gap = (rows[0] + 1..rows[1])
        .next()
        .expect("fixture needs a gap between the first two hits");

    app.cursor = gap;
    press(&mut app, "n");
    assert_eq!(
        app.cursor, rows[1],
        "n takes the first hit at or after the cursor"
    );

    app.cursor = gap;
    press(&mut app, "N");
    assert_eq!(
        app.cursor, rows[0],
        "N takes the last hit at or before the cursor"
    );

    app.cursor = rows[rows.len() - 1] + 1;
    press(&mut app, "n");
    assert_eq!(
        app.cursor, rows[0],
        "n past the final hit wraps to the first"
    );

    app.cursor = rows[0].saturating_sub(1);
    press(&mut app, "N");
    assert_eq!(
        app.cursor,
        rows[rows.len() - 1],
        "N before the first hit wraps to the last"
    );
}

#[test]
fn brace_motions_find_the_nearest_comment_from_an_unanchored_row() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    let row = app.cursor;
    assert!(row > 0, "the fixture thread needs a line above it");

    app.cursor = row + 1;
    app.focused_card = None;
    press(&mut app, "{");
    assert_eq!(app.cursor, row, "{{ reaches back to the comment above");
    assert_eq!(app.focused_thread(), Some(&*thread.id));

    app.cursor = row - 1;
    app.focused_card = None;
    press(&mut app, "}");
    assert_eq!(app.cursor, row, "}} reaches forward to the comment below");
    assert_eq!(app.focused_thread(), Some(&*thread.id));
}

#[test]
fn both_prompts_step_with_arrows_and_control_keys() {
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), Modifiers::CONTROL);

    let mut app = load();
    app.pane = Pane::Files;
    let mut input = InputRouter::default();

    press(&mut app, "/auth_check");
    assert_eq!(app.filtered_file_indices(), vec![2, 3]);

    // The arrows step; ctrl-p/n is recall, covered separately.
    for (key, expected) in [
        (KeyEvent::from(KeyCode::Up), 2),
        (KeyEvent::from(KeyCode::Down), 3),
    ] {
        send(&mut input, &mut app, key);
        assert_eq!(app.selected_file, expected, "{key:?} steps the filter");
    }

    send(&mut input, &mut app, ctrl('['));
    assert_eq!(app.mode, Mode::Normal, "ctrl-[ cancels the filter prompt");
    assert!(app.file_filter.is_none());

    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    let rows: Vec<usize> = found(&app).iter().map(Match::row).collect();

    // The arrows step the hits; ctrl-p/n is recall, covered separately.
    for (key, expected) in [
        (KeyEvent::from(KeyCode::Down), rows[1]),
        (KeyEvent::from(KeyCode::Up), rows[0]),
    ] {
        send(&mut input, &mut app, key);
        assert_eq!(app.cursor, expected, "{key:?} steps the search prompt");
    }

    send(&mut input, &mut app, ctrl('['));
    assert_eq!(app.mode, Mode::Normal, "ctrl-[ cancels the search prompt");
    assert!(app.search.is_none());
}

/// Every prompt opens clean, so recall is the only way back to an earlier one.
/// All three walk the same history path, so one test holds them to it.
#[test]
fn every_prompt_recalls_what_was_typed_into_it() {
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), Modifiers::CONTROL);

    // (opening key, two entries to run, the pane the prompt belongs to)
    let prompts: [(char, [&str; 2], Pane); 3] = [
        ('/', ["cobra", "bundle"], Pane::Diff),
        ('/', ["auth", "verify"], Pane::Files),
        (':', ["noh", "7"], Pane::Diff),
    ];

    for (open, entries, pane) in prompts {
        let mut app = load();
        app.pane = pane;
        let mut input = InputRouter::default();

        for entry in entries {
            send(
                &mut input,
                &mut app,
                KeyEvent::new(KeyCode::Char(open), Modifiers::NONE),
            );
            paste(&mut input, &mut app, entry);
            send(&mut input, &mut app, KeyCode::Enter.into());
        }

        send(
            &mut input,
            &mut app,
            KeyEvent::new(KeyCode::Char(open), Modifiers::NONE),
        );
        assert_eq!(
            prompt_text(&app),
            Some(String::new()),
            "{open} in {pane:?} opens clean"
        );

        send(&mut input, &mut app, ctrl('p'));
        assert_eq!(prompt_text(&app).as_deref(), Some(entries[1]));
        send(&mut input, &mut app, ctrl('p'));
        assert_eq!(prompt_text(&app).as_deref(), Some(entries[0]));
        send(&mut input, &mut app, ctrl('n'));
        assert_eq!(prompt_text(&app).as_deref(), Some(entries[1]));

        // Typing moves off the recall, so the next one starts from the end.
        paste(&mut input, &mut app, "x");
        send(&mut input, &mut app, ctrl('p'));
        assert_eq!(prompt_text(&app).as_deref(), Some(entries[1]));
    }
}

/// Whichever line is open, read as text.
fn prompt_text(app: &App) -> Option<String> {
    match app.mode {
        Mode::Filter => app.filter_query(),
        Mode::Search => app.search_query().map(str::to_owned),
        Mode::CommandLine => app.command_line.as_ref().map(CommentEditor::text),
        _ => None,
    }
}

/// Cancelling used to put the previous pattern back, which made `/` feel like
/// it had not cleared anything.
#[test]
fn cancelling_a_search_leaves_no_pattern_behind() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    send(&mut input, &mut app, KeyCode::Enter.into());
    assert!(app.search.is_some());

    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    send(&mut input, &mut app, KeyCode::Escape.into());

    assert!(app.search.is_none(), "the old pattern does not come back");
    assert_eq!(app.mode, Mode::Normal);
}

/// Each draft is filed against the pending review as it is written, so the
/// first one opens that review and the rest join it.
#[test]
fn every_draft_is_filed_against_one_pending_review() {
    let mut app = load();
    let added: Vec<usize> = app.files[app.selected_file]
        .lines
        .iter()
        .enumerate()
        .filter(|(_, l)| l.kind == prtui_core::LineKind::Added)
        .map(|(row, _)| row)
        .collect();

    app.cursor = added[0];
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("first");
    act(&mut app, &Action::CommitComment);

    let opening = app.take_requests();
    let Request::AddThread { draft, thread } = &opening[0] else {
        panic!("expected a draft request, got {:?}", opening[0]);
    };

    assert_eq!(thread.parent, Parent::PullRequest("PR_fixture".into()));
    assert_eq!(thread.body, "first");
    let anchor = thread.anchor.expect("line comment has an anchor");
    assert_eq!(anchor.side, Side::Right);
    assert!(
        !anchor.is_multiline(),
        "a single-line comment sends no span"
    );

    app.finish(Ok(Sent::ThreadAdded {
        draft: *draft,
        review: "PRR_1".into(),
        comment: "PRRC_1".into(),
    }));
    assert_eq!(app.drafts[0].sync, Sync::Synced);

    // Well clear of the first draft, so this is a second one and not a revision.
    app.cursor = *added.iter().find(|row| **row > added[0] + 4).unwrap();
    press(&mut app, "V2jc");
    app.composer.as_mut().unwrap().editor.set_text("spanning");
    act(&mut app, &Action::CommitComment);

    let joining = app.take_requests();
    let Request::AddThread { thread, .. } = &joining[0] else {
        panic!("expected a draft request, got {:?}", joining[0]);
    };

    assert_eq!(
        thread.parent,
        Parent::Review("PRR_1".into()),
        "joins the review"
    );
    let anchor = thread.anchor.expect("line comment has an anchor");
    assert!(anchor.start_line < anchor.end_line);
    assert_eq!(anchor.start_side, Side::Right);
}

/// Everything the review carries is already on GitHub, so submitting sends a
/// verdict against the pending review and nothing else.
#[test]
fn submitting_publishes_the_pending_review() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("first");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    press(&mut app, "s");
    assert_eq!(app.mode, Mode::Submit);
    app.submission
        .as_mut()
        .unwrap()
        .editor
        .set_text("looks good");
    act(&mut app, &Action::CycleEvent(1));
    act(&mut app, &Action::CommitSubmit);

    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.in_flight, 1);
    assert_eq!(
        app.take_requests(),
        vec![Request::Review {
            parent: Parent::Review("PRR_1".into()),
            event: ReviewEvent::Approve,
            body: "looks good".into(),
        }]
    );

    // The drafts only retire once GitHub has actually taken them.
    assert_eq!(app.drafts.len(), 1);
    app.finish(Ok(Sent::Review));
    assert!(app.drafts.is_empty());
    assert_eq!(app.in_flight, 0);
}

/// A draft still in flight would not be part of the review it is meant for, so
/// the verdict waits for it rather than publishing without it.
#[test]
fn submitting_waits_for_a_draft_still_saving() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("hold on");
    act(&mut app, &Action::CommitComment);
    app.take_requests();

    press(&mut app, "s");
    app.submission.as_mut().unwrap().editor.set_text("summary");
    act(&mut app, &Action::CommitSubmit);

    assert_eq!(app.status, "a draft is still saving");
    assert!(app.take_requests().is_empty());
    assert_eq!(app.mode, Mode::Submit);
}

#[test]
fn a_failed_submission_keeps_the_drafts() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("keep me");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    press(&mut app, "s");
    app.submission
        .as_mut()
        .unwrap()
        .editor
        .set_text("a summary");
    act(&mut app, &Action::CommitSubmit);
    assert_eq!(app.take_requests().len(), 1);

    app.finish(Err(Failure::Review(
        "HTTP 422: line must be part of the diff".into(),
    )));
    assert_eq!(app.drafts.len(), 1);
    assert!(app.status.starts_with("error:"), "{}", app.status);

    // The summary comes back with GitHub's reason attached, since the status
    // bar shows one line of it and the reason is what has to be acted on.
    let submission = app.submission.as_ref().expect("the overlay is back");
    assert_eq!(app.mode, Mode::Submit);
    assert_eq!(submission.editor.text(), "a summary");
    assert_eq!(
        submission.error.as_deref(),
        Some("HTTP 422: line must be part of the diff")
    );
}

/// Reopening over an editor would take the keyboard away mid-word, so the
/// rejected review waits for the next `s` instead.
#[test]
fn a_rejection_mid_edit_holds_the_summary_until_asked_for() {
    let mut app = load();
    park_on_code(&mut app);

    press(&mut app, "s");
    choose(&mut app, ReviewEvent::RequestChanges);
    app.submission.as_mut().unwrap().editor.set_text("fix this");
    act(&mut app, &Action::CommitSubmit);
    app.take_requests();

    press(&mut app, "c");
    app.finish(Err(Failure::Review("HTTP 422: nope".into())));
    assert!(app.submission.is_none(), "the composer keeps the keyboard");
    assert_eq!(app.mode, Mode::Insert);

    act(&mut app, &Action::CancelComment);
    press(&mut app, "s");
    let submission = app.submission.as_ref().expect("the overlay is back");
    assert_eq!(submission.editor.text(), "fix this");
    assert_eq!(submission.event, ReviewEvent::RequestChanges);
    assert_eq!(submission.error.as_deref(), Some("HTTP 422: nope"));
}

/// The drafts only retire when GitHub answers, so a second review sent before
/// the first is answered would ship every one of them twice.
#[test]
fn a_second_review_waits_for_the_one_in_flight() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("once");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    press(&mut app, "s");
    app.submission.as_mut().unwrap().editor.set_text("summary");
    act(&mut app, &Action::CommitSubmit);
    assert_eq!(app.take_requests().len(), 1);

    press(&mut app, "s");
    app.submission.as_mut().unwrap().editor.set_text("again");
    act(&mut app, &Action::CommitSubmit);

    assert_eq!(app.status, "a review is already going out");
    assert!(app.take_requests().is_empty());
    assert_eq!(app.mode, Mode::Submit, "the overlay keeps what was typed");
    assert_eq!(app.in_flight, 1);
}

/// A verdict with nothing under it has no pending review to publish, so it
/// files and submits one against the pull request itself.
#[test]
fn a_bare_approval_needs_neither_summary_nor_comments() {
    let mut app = load();
    press(&mut app, "s");

    // Approving is the one verdict GitHub takes without a summary, and it is a
    // verdict on its own, so nothing else has to accompany it.
    choose(&mut app, ReviewEvent::Approve);
    act(&mut app, &Action::CommitSubmit);

    assert!(app.drafts.is_empty());
    assert_eq!(app.in_flight, 1);
    assert_eq!(
        app.take_requests(),
        vec![Request::Review {
            parent: Parent::PullRequest("PR_fixture".into()),
            event: ReviewEvent::Approve,
            body: String::new(),
        }]
    );
}

#[test]
fn a_verdict_that_carries_prose_is_refused_without_it() {
    for event in [ReviewEvent::Comment, ReviewEvent::RequestChanges] {
        let label = event.label();
        let mut app = load();
        press(&mut app, "s");
        choose(&mut app, event);
        act(&mut app, &Action::CommitSubmit);

        assert_eq!(app.status, format!("{label} needs a summary"));
        assert_eq!(app.in_flight, 0);
        assert!(app.take_requests().is_empty());
        // The overlay stays open so the summary is typed, not retyped.
        assert!(app.submission.is_some());
    }
}

/// GitHub answers a blank `COMMENT` or `REQUEST_CHANGES` with a bare 422, so
/// the overlay catches it first and keeps what was typed.
#[test]
fn a_verdict_that_needs_a_summary_says_so_before_sending() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer
        .as_mut()
        .unwrap()
        .editor
        .set_text("inline only");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    for (event, label) in [
        (ReviewEvent::Comment, "comment needs a summary"),
        (
            ReviewEvent::RequestChanges,
            "request changes needs a summary",
        ),
    ] {
        press(&mut app, "s");
        while app.submission.as_ref().unwrap().event != event {
            act(&mut app, &Action::CycleEvent(1));
        }

        act(&mut app, &Action::CommitSubmit);
        assert_eq!(app.status, label);
        assert_eq!(app.mode, Mode::Submit, "the overlay stays open to type in");
        assert!(app.take_requests().is_empty());
        assert_eq!(app.drafts.len(), 1, "the draft is untouched");

        act(&mut app, &Action::CancelSubmit);
    }

    // Approving carries the same draft with no summary at all.
    press(&mut app, "s");
    act(&mut app, &Action::CycleEvent(1));
    act(&mut app, &Action::CommitSubmit);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.in_flight, 1);
    assert!(matches!(
        app.take_requests().as_slice(),
        [Request::Review {
            event: ReviewEvent::Approve,
            ..
        }]
    ));
}

#[test]
fn commenting_on_a_focused_thread_replies_to_it() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    press(&mut app, "j");
    assert_eq!(app.focused_thread(), Some(&*thread.id));

    press(&mut app, "c");
    assert_eq!(app.mode, Mode::Insert);
    app.composer.as_mut().unwrap().editor.set_text("good catch");
    act(&mut app, &Action::CommitComment);

    assert!(app.drafts.is_empty(), "a reply is not a review draft");
    assert_eq!(
        app.take_requests(),
        vec![Request::Reply {
            in_reply_to: thread.reply_target().unwrap(),
            body: "good catch".into(),
        }]
    );
    assert_eq!(app.in_flight, 1);

    app.finish(Ok(Sent::Reply));
    assert_eq!(app.status, "reply posted");
}

#[test]
fn resolving_toggles_the_focused_thread() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    press(&mut app, "j");

    press(&mut app, "R");
    assert_eq!(
        app.take_requests(),
        vec![Request::Resolve {
            thread_id: thread.id,
            is_resolved: true,
        }]
    );

    act(&mut app, &Action::LeaveThread);
    press(&mut app, "R");
    assert_eq!(app.status, "no thread selected");
    assert!(app.take_requests().is_empty());
}

/// The mark is the server's, so a toggle sends the opposite of what the last
/// metadata fetch reported and waits for the refetch to say it landed.
///
/// Marking read also opens the next file, the way `]` would. Clearing the mark
/// does not: that is a reader coming back to the file, not leaving it.
#[test]
fn marking_a_file_viewed_toggles_it_and_steps_on() {
    let mut app = load();
    app.selected_file = 0;
    let path = app.files[0].path.clone();

    press(&mut app, "x");
    assert_eq!(
        app.take_requests(),
        vec![Request::SetViewed {
            pr: "PR_fixture".into(),
            path: path.clone(),
            is_viewed: true,
        }]
    );
    assert_eq!(app.selected_file, 1);

    // The mark is the app's once GitHub confirms it: no metadata fetch has to
    // land for the tick to show.
    app.finish(Ok(Sent::Viewed {
        path: path.clone(),
        is_viewed: true,
    }));
    assert_eq!(app.status, "file marked viewed");
    assert!(app.tree_row(0).unwrap().is_viewed);

    app.selected_file = 0;

    press(&mut app, "x");
    assert_eq!(
        app.take_requests(),
        vec![Request::SetViewed {
            pr: "PR_fixture".into(),
            path,
            is_viewed: false,
        }]
    );
    assert_eq!(app.selected_file, 0);
}

/// A file already marked is stepped over, not landed on: `x` there would clear
/// its mark, so walking the review with `x` would undo the last session.
#[test]
fn marking_a_file_viewed_steps_over_the_ones_already_read() {
    let mut app = load();
    app.set_meta(meta_marking_viewed(&app.files[1].path.clone()));
    app.selected_file = 0;

    press(&mut app, "x");

    assert_eq!(app.selected_file, 2);
}

/// The walk down the review is a lap: the file after the last one is the
/// first file still unread, not a dead end.
#[test]
fn marking_the_last_file_comes_round_to_the_first_unread() {
    let mut app = load();
    let last = app.files.len() - 1;
    app.selected_file = last;

    press(&mut app, "x");

    assert_eq!(app.take_requests().len(), 1);
    assert_eq!(app.selected_file, 0);
}

/// Nothing left unread is not a failure, but the reader pressed a key and did
/// not move, so the bar says why.
#[test]
fn marking_the_only_unread_file_stays_on_it_and_says_so() {
    let mut app = load();
    let last = app.files.len() - 1;
    mark_viewed(&mut app, &(0..last).collect::<Vec<_>>());
    app.selected_file = last;

    press(&mut app, "x");

    assert_eq!(app.take_requests().len(), 1);
    assert_eq!(app.selected_file, last);
    assert_eq!(app.status, "marking viewed… nothing left unread");
}

/// Takes the review's own marks over, the way a confirmation from GitHub
/// would, so a test can say which files have been read through.
fn mark_viewed(app: &mut App, files: &[usize]) {
    for &index in files {
        let path = app.files[index].path.clone();
        app.finish(Ok(Sent::Viewed {
            path,
            is_viewed: true,
        }));
    }
    app.take_requests();
    app.status.clear();
}

/// The fixture as GitHub would send it back once `path` has been read through.
fn meta_marking_viewed(path: &str) -> prtui_core::Meta {
    let mut meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();
    let files = meta["data"]["repository"]["pullRequest"]["files"]["nodes"]
        .as_array_mut()
        .unwrap();

    for file in files.iter_mut().filter(|file| file["path"] == path) {
        file["viewerViewedState"] = "VIEWED".into();
    }

    parse_meta(&serde_json::to_vec(&meta).unwrap()).unwrap()
}

#[test]
fn e_reopens_the_draft_instead_of_stacking_another() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("first pass");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);
    let rows = app.drafts[0].rows().unwrap().clone();

    press(&mut app, "e");
    assert_eq!(
        app.composer.as_ref().unwrap().editor.text(),
        "first pass",
        "the composer reopens the draft it will replace"
    );
    app.composer
        .as_mut()
        .unwrap()
        .editor
        .set_text("second pass");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    assert_eq!(app.drafts.len(), 1);
    assert_eq!(app.drafts[0].body, "second pass");
    assert_eq!(
        *app.drafts[0].rows().unwrap(),
        rows,
        "editing keeps the original span"
    );

    // Emptying a reopened draft is how it gets thrown away.
    press(&mut app, "e");
    app.composer.as_mut().unwrap().editor.set_text("");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);
    assert!(app.drafts.is_empty());

    press(&mut app, "e");
    assert_eq!(app.status, "no draft here");
    assert!(app.composer.is_none());
}

/// `c` composes, `e` revises. Commenting a drafted line again is a second
/// comment, which GitHub allows and the old contextual `c` quietly prevented.
#[test]
fn c_always_starts_a_new_comment() {
    let mut app = load();
    park_on_code(&mut app);

    for body in ["one", "two"] {
        press(&mut app, "c");
        app.composer.as_mut().unwrap().editor.set_text(body);
        act(&mut app, &Action::CommitComment);
    }

    assert_eq!(app.drafts.len(), 2);
    assert_eq!(app.drafts[1].body, "two");
}

#[test]
fn d_discards_only_the_draft_under_the_cursor() {
    let mut app = load();
    park_on_code(&mut app);
    let first = app.cursor;

    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("one");
    act(&mut app, &Action::CommitComment);

    app.cursor = first + 1;
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("two");
    act(&mut app, &Action::CommitComment);
    assert_eq!(app.drafts.len(), 2);

    press(&mut app, "d");
    assert_eq!(app.status, "draft discarded");
    assert_eq!(app.drafts.len(), 1);
    assert_eq!(app.drafts[0].body, "one");

    app.cursor = first + 1;
    press(&mut app, "d");
    assert_eq!(app.status, "no draft here");
    assert_eq!(app.drafts.len(), 1);
}

#[test]
fn the_submit_overlay_types_its_summary_and_tabs_the_verdict() {
    let mut app = load();
    press(&mut app, "s");

    let mut input = InputRouter::default();
    send(&mut input, &mut app, KeyCode::Tab.into());
    assert_eq!(app.submission.as_ref().unwrap().event, ReviewEvent::Approve);
    send(&mut input, &mut app, KeyCode::BackTab.into());
    assert_eq!(app.submission.as_ref().unwrap().event, ReviewEvent::Comment);

    // Plain keys belong to the summary, including ones bound in normal mode.
    press(&mut app, "ship it");
    assert_eq!(app.submission.as_ref().unwrap().editor.text(), "ship it");
    assert_eq!(app.mode, Mode::Submit);

    // Shifted Enter breaks the line; the bare one sends.
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Enter, Modifiers::SHIFT),
    );
    assert_eq!(app.submission.as_ref().unwrap().editor.text(), "ship it\n");

    // A typed summary is no cheaper to retype than a comment, so the first
    // escape only warns.
    send(&mut input, &mut app, KeyEvent::from(KeyCode::Escape));
    assert_eq!(app.mode, Mode::Submit);
    assert_eq!(app.status, "esc again to discard");

    send(&mut input, &mut app, KeyEvent::from(KeyCode::Escape));
    assert_eq!(app.mode, Mode::Normal);
    assert!(app.submission.is_none());

    press(&mut app, "s");
    app.submission.as_mut().unwrap().editor.set_text("ship it");
    send(&mut input, &mut app, KeyEvent::from(KeyCode::Enter));
    assert_eq!(app.mode, Mode::Normal, "enter sends the review");
    assert_eq!(app.in_flight, 1);
}

/// A draft is on screen before GitHub has taken it, so the gutter has to say
/// which of the two the reader is looking at.
#[test]
fn a_draft_reports_how_far_it_has_got() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("wip");
    act(&mut app, &Action::CommitComment);

    assert_eq!(app.drafts[0].sync, Sync::Creating { is_dirty: false });
    assert_eq!(app.status, "saving draft…");

    let requests = app.take_requests();
    let Request::AddThread { draft, .. } = requests[0] else {
        panic!("expected a draft request, got {:?}", requests[0]);
    };

    app.finish(Err(Failure::Draft(draft, "HTTP 422: nope".into())));
    assert_eq!(app.drafts[0].sync, Sync::Failed("HTTP 422: nope".into()));
    assert_eq!(app.drafts[0].body, "wip", "the writing is not thrown away");
}

/// The first draft opens the pending review. A second sent beside it would open
/// a second review, so it waits for the id the first one comes back with.
#[test]
fn drafts_written_together_share_one_review() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("one");
    act(&mut app, &Action::CommitComment);

    let opening = app.take_requests();
    assert_eq!(opening.len(), 1);

    app.cursor += 1;
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("two");
    act(&mut app, &Action::CommitComment);

    assert!(
        app.take_requests().is_empty(),
        "the second waits for the review the first opens"
    );
    assert_eq!(app.drafts[1].sync, Sync::Queued);

    let Request::AddThread { draft, .. } = opening[0] else {
        panic!("expected a draft request");
    };
    app.finish(Ok(Sent::ThreadAdded {
        draft,
        review: "PRR_1".into(),
        comment: "PRRC_1".into(),
    }));

    let joining = app.take_requests();
    assert!(matches!(
        joining.as_slice(),
        [Request::AddThread { thread, .. }]
            if matches!(&thread.parent, Parent::Review(_))
    ));
}

/// An edit that beats its own creation home has no comment id to name, so it
/// rides on that answer rather than being lost or sent to nothing.
#[test]
fn an_edit_before_the_creation_lands_follows_it() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("first");
    act(&mut app, &Action::CommitComment);

    let requests = app.take_requests();
    let Request::AddThread { draft, .. } = requests[0] else {
        panic!("expected a draft request");
    };

    press(&mut app, "e");
    app.composer.as_mut().unwrap().editor.set_text("revised");
    act(&mut app, &Action::CommitComment);

    assert_eq!(app.drafts[0].sync, Sync::Creating { is_dirty: true });
    assert!(app.take_requests().is_empty(), "nothing to address it to");

    app.finish(Ok(Sent::ThreadAdded {
        draft,
        review: "PRR_1".into(),
        comment: "PRRC_1".into(),
    }));

    assert_eq!(
        app.take_requests(),
        vec![Request::UpdateComment {
            draft,
            comment: "PRRC_1".into(),
            body: "revised".into(),
        }]
    );
}

/// Same for a discard: the comment has to exist before it can be dropped.
#[test]
fn a_discard_before_the_creation_lands_follows_it() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("nevermind");
    act(&mut app, &Action::CommitComment);

    let requests = app.take_requests();
    let Request::AddThread { draft, .. } = requests[0] else {
        panic!("expected a draft request");
    };

    act(&mut app, &Action::DeleteDraft);
    assert_eq!(app.drafts[0].sync, Sync::Deleting);
    assert!(app.take_requests().is_empty());

    app.finish(Ok(Sent::ThreadAdded {
        draft,
        review: "PRR_1".into(),
        comment: "PRRC_1".into(),
    }));

    assert_eq!(
        app.take_requests(),
        vec![Request::DeleteComment {
            draft,
            comment: "PRRC_1".into(),
        }]
    );

    app.finish(Ok(Sent::CommentDeleted(draft)));
    assert!(app.drafts.is_empty());
}

/// A metadata fetch that left before a discard landed still carries the comment
/// it dropped. Trusting it would put the draft back on screen.
#[test]
fn a_discarded_draft_does_not_come_back_from_a_stale_fetch() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("gone");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    let comment = app.drafts[0].remote.clone().unwrap();
    act(&mut app, &Action::DeleteDraft);
    settle(&mut app);
    assert!(app.drafts.is_empty());

    app.set_meta(meta_with_pending(&comment, "gone"));
    assert!(
        app.drafts.is_empty(),
        "the discard outranks the stale fetch"
    );
}

/// The screen is the newer of the two while a write is out, so a fetch that
/// predates it must not undo an edit in front of the user.
#[test]
fn a_fetch_does_not_overwrite_a_draft_still_saving() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("first");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    let comment = app.drafts[0].remote.clone().unwrap();
    press(&mut app, "e");
    app.composer.as_mut().unwrap().editor.set_text("revised");
    act(&mut app, &Action::CommitComment);
    assert_eq!(app.drafts[0].sync, Sync::Updating);

    app.set_meta(meta_with_pending(&comment, "first"));

    assert_eq!(app.drafts.len(), 1);
    assert_eq!(app.drafts[0].body, "revised");
    assert_eq!(app.drafts[0].sync, Sync::Updating);
}

/// A refetch lands after every write. It rebuilds the drafts from what GitHub
/// reported, so a draft that keeps its comment has to keep the local id the
/// focus and a reopened composer address it by.
#[test]
fn a_refetch_keeps_the_focus_on_the_same_draft() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("first");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    let comment = app.drafts[0].remote.clone().unwrap();
    let focused = app.focused_card.clone();
    assert_eq!(focused, Some(Card::Draft(app.drafts[0].id)));

    app.set_meta(meta_with_pending(&comment, "first"));

    assert_eq!(app.drafts.len(), 1);
    assert_eq!(app.focused_card, focused, "the cursor stays on the card");
    assert_eq!(app.focused_card, Some(Card::Draft(app.drafts[0].id)));
}

/// A draft is a card the cursor walks the same way it walks a conversation:
/// down onto it from the line it hangs under, and back off the top of it.
#[test]
fn the_cursor_walks_drafts_like_any_other_card() {
    let mut app = load();
    park_on_code(&mut app);
    let row = app.cursor;

    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("a note");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    let card = app.focused_card.clone().unwrap();
    press(&mut app, "k");
    assert!(app.focused_card.is_none(), "k steps off the card");
    assert_eq!(app.cursor, row);

    press(&mut app, "j");
    assert_eq!(app.focused_card, Some(card), "j steps back onto it");

    act(&mut app, &Action::Activate);
    assert_eq!(app.expanded_card, app.focused_card);
}

/// `}` walks what a review still owes an answer to, and an unsent note of the
/// reader's own is the plainest example of one.
#[test]
fn jumping_between_comments_stops_on_drafts() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    app.composer.as_mut().unwrap().editor.set_text("a note");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    let card = app.focused_card.clone().unwrap();
    app.cursor = 0;
    act(&mut app, &Action::LeaveThread);

    act(&mut app, &Action::NextComment(1));
    assert_eq!(app.focused_card, Some(card));
}

/// The fixture's metadata with one pending thread bolted on, standing in for a
/// fetch that still believes in a draft.
fn meta_with_pending(comment: &str, body: &str) -> prtui_core::Meta {
    let mut meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();
    let pr = &mut meta["data"]["repository"]["pullRequest"];

    pr["pendingReview"] = serde_json::json!({ "nodes": [{ "id": "PRR_1" }] });
    let mut thread = pr["reviewThreads"]["nodes"][0].clone();
    thread["id"] = "PRRT_pending".into();
    thread["comments"]["nodes"] = serde_json::json!([{
        "id": comment,
        "state": "PENDING",
        "fullDatabaseId": null,
        "author": { "login": "tale" },
        "body": body,
        "createdAt": "now",
    }]);
    pr["reviewThreads"]["nodes"] = serde_json::json!([thread]);

    parse_meta(&serde_json::to_vec(&meta).unwrap()).unwrap()
}

/// The reference is a fixed-column table, so the columns have to be wide
/// enough for what the keymap and the command table actually hold.
#[test]
fn the_reference_fits_the_columns_it_is_drawn_in() {
    use prtui_tui::app::keymap::Reference;

    let app = load();
    for line in app.keymap().reference() {
        let Reference::Entry {
            keys,
            name,
            summary,
        } = line
        else {
            continue;
        };

        assert!(keys.chars().count() <= 18, "keys column: {keys}");
        assert!(name.chars().count() <= 19, "name column: {name}");
        assert!(summary.chars().count() <= 36, "summary column: {summary}");
    }
}

/// A search stays highlighted while the tree has the focus, and used to be
/// unclearable from there: escape asked "is a find live", `clear_find` asked
/// "which pane is this", and the two disagreed until the key did nothing.
#[test]
fn escape_clears_the_search_from_either_pane() {
    for pane in [Pane::Diff, Pane::Files] {
        let mut app = load();
        let mut input = search_for(&mut app, "cobra");
        send(&mut input, &mut app, KeyCode::Enter.into());
        act(&mut app, &Action::LeaveThread);
        app.pane = pane;
        assert!(app.search.is_some());

        send(&mut input, &mut app, KeyCode::Escape.into());
        assert!(
            app.search.is_none(),
            "one escape should clear it in {pane:?}"
        );
        assert!(!app.should_quit);
    }
}

/// With both live the pane decides the order, so the tree's own filter goes
/// first and the search survives to be cleared next.
#[test]
fn the_focused_pane_picks_which_find_clears_first() {
    // Filter first: the other order clears the search as the filter opens.
    let mut app = load();
    app.pane = Pane::Files;
    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    paste(&mut input, &mut app, "auth_check");
    send(&mut input, &mut app, KeyCode::Enter.into());

    let mut input = search_for(&mut app, "cobra");
    send(&mut input, &mut app, KeyCode::Enter.into());
    act(&mut app, &Action::LeaveThread);
    app.pane = Pane::Files;
    assert!(app.file_filter.is_some() && app.search.is_some());

    send(&mut input, &mut app, KeyCode::Escape.into());
    assert!(
        app.file_filter.is_none(),
        "the tree's own filter goes first"
    );
    assert!(app.search.is_some());

    send(&mut input, &mut app, KeyCode::Escape.into());
    assert!(app.search.is_none());
}

/// The diff keeps its highlights while the tree has the focus, so a filter
/// started after a search used to open over a screen still lit by it.
#[test]
fn filtering_the_tree_drops_a_search_left_in_the_diff() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    send(&mut input, &mut app, KeyCode::Enter.into());
    act(&mut app, &Action::LeaveThread);

    send(&mut input, &mut app, KeyEvent::from(KeyCode::Tab));
    assert_eq!(app.pane, Pane::Files);
    assert!(app.search.is_some(), "tabbing alone leaves it alone");

    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    assert_eq!(app.mode, Mode::Filter);
    assert!(app.search.is_none(), "`/` in the tree clears the search");
}

/// The reverse does not hold: a filter is the set of files being reviewed, not
/// decoration, so searching inside one of them keeps it.
#[test]
fn searching_a_file_keeps_the_tree_filtered() {
    let mut app = load();
    app.pane = Pane::Files;

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    paste(&mut input, &mut app, "auth_check");
    send(&mut input, &mut app, KeyCode::Enter.into());
    assert!(app.file_filter.is_some());

    let mut input = search_for(&mut app, "cobra");
    send(&mut input, &mut app, KeyCode::Enter.into());

    assert!(app.file_filter.is_some(), "the filtered set survives");
    assert!(app.search.is_some());
}
