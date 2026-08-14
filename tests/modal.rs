use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prtui::app::action::{Action, Motion};
use prtui::app::draft::Side;
use prtui::app::input::{DispatchResult, InputRouter};
use prtui::app::keymap::{Keymap, Resolution};
use prtui::app::mode::Mode;
use prtui::app::{App, Pane};
use prtui::model::{parse_files, parse_meta};

fn load() -> App {
    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    let meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();

    let mut app = App::new();
    app.files = parse_files(&files).unwrap();
    app.set_meta(parse_meta(&meta).unwrap());
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
        .position(|l| l.kind != prtui::model::LineKind::Hunk)
        .unwrap();
}

fn press(app: &mut App, keys: &str) {
    let mut input = InputRouter::default();
    for c in keys.chars() {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        input.dispatch_key(app, key, 20);
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

#[test]
fn gg_needs_both_keys() {
    let mut app = load();
    press(&mut app, "9j");
    assert_eq!(app.cursor, 9);

    // A lone `g` is incomplete and must not move the cursor.
    let mut input = InputRouter::default();
    let key = KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE);
    assert_eq!(input.dispatch_key(&mut app, key, 20), DispatchResult::Pending);
    assert_eq!(app.cursor, 9);
    assert_eq!(input.pending_hint(), "g");

    assert_eq!(
        input.dispatch_key(&mut app, key, 20),
        DispatchResult::Applied(Action::Move(Motion::Top))
    );
    assert_eq!(app.cursor, 0);
    assert!(input.pending_hint().is_empty());
}

#[test]
fn leading_zero_is_unbound_and_does_not_start_a_count() {
    let mut app = load();
    let mut keymap = Keymap::default();
    let key = KeyEvent::new(KeyCode::Char('0'), KeyModifiers::NONE);

    // Leading zero must not start a count that swallows the next motion.
    assert_eq!(keymap.resolve(Mode::Normal, key), Resolution::Unbound);
    press(&mut app, "j");
    assert_eq!(app.cursor, 1);
}

#[test]
fn ctrl_d_is_a_half_page() {
    let mut keymap = Keymap::default();
    let key = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);

    assert_eq!(
        keymap.resolve(Mode::Normal, key),
        Resolution::Action(Action::Move(Motion::HalfPageDown))
    );
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
        .position(|l| l.kind == prtui::model::LineKind::Added)
        .unwrap();
    app.cursor = added;

    press(&mut app, "V");
    press(&mut app, "2j");
    press(&mut app, "c");
    assert_eq!(app.mode, Mode::Insert);
    assert!(app.composer.is_some());

    let composer = app.composer.as_mut().unwrap();
    composer.editor.lines = edtui::Lines::from("this allocates on every call");

    app.apply(Action::CommitComment, 20);

    assert_eq!(app.mode, Mode::Normal);
    assert!(app.selection.is_none());
    assert_eq!(app.drafts.len(), 1);

    let draft = &app.drafts[0];
    assert_eq!(draft.side, Side::Right);
    assert_eq!(draft.body, "this allocates on every call");
    assert!(draft.is_multiline());
    assert!(draft.start_line < draft.end_line);
}

#[test]
fn an_empty_comment_is_discarded() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");
    assert_eq!(app.mode, Mode::Insert);

    app.apply(Action::CommitComment, 20);
    assert!(app.drafts.is_empty());
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn cancelling_keeps_the_buffer_out_of_the_drafts() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");

    let composer = app.composer.as_mut().unwrap();
    composer.editor.lines = edtui::Lines::from("never mind");

    let mut input = InputRouter::default();
    let cancel = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
    assert_eq!(
        input.dispatch_key(&mut app, cancel, 20),
        DispatchResult::Applied(Action::CancelComment)
    );
    assert!(app.composer.is_none());
    assert!(app.drafts.is_empty());
    assert_eq!(app.mode, Mode::Normal);
}

#[test]
fn insert_mode_reserves_only_the_submit_and_cancel_chords() {
    let mut app = load();
    park_on_code(&mut app);
    press(&mut app, "c");

    let mut input = InputRouter::default();

    // A bare letter belongs to the editor widget, not the app keymap.
    let letter = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE);
    assert_eq!(
        input.dispatch_key(&mut app, letter, 20),
        DispatchResult::ForwardedToEditor
    );
    assert_eq!(app.composer.as_ref().unwrap().editor.lines.to_string(), "s");

    // Extra modifiers do not accidentally trigger an application chord.
    let modified = KeyEvent::new(
        KeyCode::Char('s'),
        KeyModifiers::CONTROL | KeyModifiers::ALT,
    );
    assert_eq!(
        input.dispatch_key(&mut app, modified, 20),
        DispatchResult::ForwardedToEditor
    );
    assert!(app.composer.is_some());

    let chord = KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL);
    assert_eq!(
        input.dispatch_key(&mut app, chord, 20),
        DispatchResult::Applied(Action::CommitComment)
    );
    assert!(app.composer.is_none());
    assert_eq!(app.drafts[0].body, "s");
}

#[test]
fn paste_is_routed_only_to_an_open_composer() {
    let mut app = load();
    let mut input = InputRouter::default();

    assert_eq!(
        input.dispatch_paste(&mut app, "ignored".into()),
        DispatchResult::Ignored
    );

    park_on_code(&mut app);
    press(&mut app, "c");
    assert_eq!(
        input.dispatch_paste(&mut app, "pasted text".into()),
        DispatchResult::ForwardedToEditor
    );
    assert_eq!(
        app.composer.as_ref().unwrap().editor.lines.to_string(),
        "pasted text"
    );
}

#[test]
fn alt_modified_normal_bindings_are_ignored() {
    let mut app = load();
    let mut input = InputRouter::default();
    let alt_j = KeyEvent::new(KeyCode::Char('j'), KeyModifiers::ALT);

    assert_eq!(input.dispatch_key(&mut app, alt_j, 20), DispatchResult::Ignored);
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
