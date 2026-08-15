use prtui::app::action::{Action, Motion};
use prtui::app::draft::Side;
use prtui::app::input::{DispatchResult, InputRouter};
use prtui::app::keymap::{Keymap, Resolution};
use prtui::app::mode::Mode;
use prtui::app::search::Match;
use prtui::app::{App, Pane};
use prtui::model::{parse_files, parse_meta};
use termina::event::{KeyCode, KeyEvent, Modifiers};

fn load() -> App {
    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    let meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();

    let mut app = App::new();
    app.set_files(parse_files(&files).unwrap());
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

fn park_on_unresolved_thread(app: &mut App) -> prtui::model::ReviewThread {
    let thread = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();
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

fn press(app: &mut App, keys: &str) {
    let mut input = InputRouter::default();
    for c in keys.chars() {
        let key = KeyEvent::new(KeyCode::Char(c), Modifiers::NONE);
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
    let key = KeyEvent::new(KeyCode::Char('g'), Modifiers::NONE);
    assert_eq!(
        input.dispatch_key(&mut app, key, 20),
        DispatchResult::Pending
    );
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
    let key = KeyEvent::new(KeyCode::Char('0'), Modifiers::NONE);

    // Leading zero must not start a count that swallows the next motion.
    assert_eq!(
        keymap.resolve(Mode::Normal, false, key),
        Resolution::Unbound
    );
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
    assert_eq!(app.focused_thread.as_deref(), Some(first.id.as_str()));

    press(&mut app, "j");
    assert_eq!(app.cursor, anchor);
    assert_eq!(app.focused_thread.as_deref(), Some(second.id.as_str()));

    press(&mut app, "j");
    assert_eq!(app.cursor, anchor + 1);
    assert!(app.focused_thread.is_none());

    press(&mut app, "k");
    assert_eq!(app.cursor, anchor);
    assert_eq!(app.focused_thread.as_deref(), Some(second.id.as_str()));
}

#[test]
fn enter_toggles_the_focused_thread() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    press(&mut app, "j");
    assert_eq!(app.focused_thread.as_deref(), Some(thread.id.as_str()));

    let mut input = InputRouter::default();
    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    assert_eq!(app.expanded_thread.as_deref(), Some(thread.id.as_str()));

    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    assert!(app.expanded_thread.is_none());
}

#[test]
fn expanded_thread_movement_scrolls_without_losing_focus() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    press(&mut app, "j");

    let mut input = InputRouter::default();
    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    assert!(app.expanded_thread.is_some());
    app.thread_scroll_limit = 4;

    press(&mut app, "j");
    assert_eq!(app.thread_scroll, 1);
    assert_eq!(app.focused_thread.as_deref(), Some(thread.id.as_str()));
    assert_eq!(app.expanded_thread.as_deref(), Some(thread.id.as_str()));

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
        input.dispatch_key(&mut app, KeyCode::Escape.into(), 20),
        DispatchResult::Applied(Action::LeaveThread)
    );
    assert_eq!(app.cursor, source_row);
    assert!(app.focused_thread.is_none());
    assert!(!app.should_quit);
}

#[test]
fn visual_movement_remains_source_line_only() {
    let mut app = load();
    park_on_unresolved_thread(&mut app);
    let anchor = app.cursor;

    press(&mut app, "Vj");

    assert_eq!(app.cursor, anchor + 1);
    assert!(app.focused_thread.is_none());
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
    composer.editor.set_text("this allocates on every call");

    app.apply(&Action::CommitComment, 20);

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

    app.apply(&Action::CommitComment, 20);
    assert!(app.drafts.is_empty());
    assert_eq!(app.mode, Mode::Normal);
}

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
        input.dispatch_key(&mut app, cancel, 20),
        DispatchResult::Applied(Action::CancelComment)
    );
    assert!(app.composer.is_none());
    assert!(app.drafts.is_empty());
    assert_eq!(app.mode, Mode::Normal);
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
        input.dispatch_key(&mut app, letter, 20),
        DispatchResult::ForwardedToEditor
    );
    assert_eq!(app.composer.as_ref().unwrap().editor.text(), "s");

    // Extra modifiers do not accidentally trigger an application chord.
    let modified =
        KeyEvent::new(KeyCode::Char('s'), Modifiers::CONTROL | Modifiers::ALT);
    assert_eq!(
        input.dispatch_key(&mut app, modified, 20),
        DispatchResult::ForwardedToEditor
    );
    assert!(app.composer.is_some());

    let chord = KeyEvent::new(KeyCode::Char('s'), Modifiers::CONTROL);
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
        input.dispatch_paste(&mut app, "ignored", 20),
        DispatchResult::Ignored
    );

    park_on_code(&mut app);
    press(&mut app, "c");
    assert_eq!(
        input.dispatch_paste(&mut app, "pasted text", 20),
        DispatchResult::ForwardedToEditor
    );
    assert_eq!(app.composer.as_ref().unwrap().editor.text(), "pasted text");
}

#[test]
fn alt_modified_normal_bindings_are_ignored() {
    let mut app = load();
    let mut input = InputRouter::default();
    let alt_j = KeyEvent::new(KeyCode::Char('j'), Modifiers::ALT);

    assert_eq!(
        input.dispatch_key(&mut app, alt_j, 20),
        DispatchResult::Ignored
    );
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
        input.dispatch_key(&mut app, key, 20);

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
        }
        assert_eq!(app.mode, mode);

        let mut input = InputRouter::default();
        assert_eq!(
            input.dispatch_key(&mut app, ctrl_c, 20),
            DispatchResult::Applied(Action::Quit)
        );
        assert!(app.should_quit, "Ctrl+C should quit from {mode:?}");
    }
}

#[test]
fn escape_and_ctrl_bracket_quit_from_normal_mode() {
    for key in [
        KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
        KeyEvent::new(KeyCode::Char('['), Modifiers::CONTROL),
    ] {
        let mut app = load();
        let mut input = InputRouter::default();
        assert_eq!(
            input.dispatch_key(&mut app, key, 20),
            DispatchResult::Applied(Action::Quit)
        );
        assert!(app.should_quit);
    }
}

#[test]
fn ctrl_bracket_leaves_visual_mode() {
    let mut app = load();
    press(&mut app, "V");
    assert_eq!(app.mode, Mode::Visual);

    let mut input = InputRouter::default();
    input.dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('['), Modifiers::CONTROL),
        20,
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
    input.dispatch_key(&mut app, KeyCode::Tab.into(), 20);
    assert!(app.is_files_visible);
    assert_eq!(app.pane, Pane::Files);

    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    assert_eq!(app.pane, Pane::Diff);

    input.dispatch_key(&mut app, KeyCode::Left.into(), 20);
    assert_eq!(app.pane, Pane::Files);
    input.dispatch_key(&mut app, KeyCode::Right.into(), 20);
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

    input.dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('p'), Modifiers::CONTROL),
        20,
    );
    assert_eq!(app.selected_file, 2, "ctrl-p steps back through matches");

    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.filter_query().as_deref(), Some("auth_check"));

    for (keys, selected) in [("j", 3), ("gg", 2), ("G", 3)] {
        press(&mut app, keys);
        assert_eq!(
            app.selected_file, selected,
            "{keys} stays within the matches"
        );
    }

    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    assert_eq!(app.pane, Pane::Diff, "enter opens the selected match");
}

#[test]
fn escape_restores_whatever_the_filter_replaced() {
    let mut app = load();
    app.pane = Pane::Files;
    let original_file = app.selected_file;
    let mut input = InputRouter::default();

    press(&mut app, "/");
    input.dispatch_paste(&mut app, "nothing\nwill\rmatch", 20);
    assert!(app.filtered_file_indices().is_empty());

    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    assert_eq!(
        app.mode,
        Mode::Filter,
        "enter will not commit an empty result"
    );

    input.dispatch_key(&mut app, KeyCode::Escape.into(), 20);
    assert!(app.file_filter.is_none());
    assert_eq!(app.selected_file, original_file);

    // Escaping an edit rewinds to the committed query, not to no filter at all.
    press(&mut app, "/auth_check");
    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    app.selected_file = 2;

    press(&mut app, "/_test");
    assert_eq!(app.filter_query().as_deref(), Some("auth_check_test"));
    assert_eq!(app.selected_file, 3, "the edit previews its narrower match");

    input.dispatch_key(&mut app, KeyCode::Escape.into(), 20);
    assert_eq!(app.filter_query().as_deref(), Some("auth_check"));
    assert_eq!(app.selected_file, 2);
}

#[test]
fn escape_clears_a_committed_filter_before_quitting() {
    for clear in [
        KeyEvent::new(KeyCode::Escape, Modifiers::NONE),
        KeyEvent::new(KeyCode::Char('['), Modifiers::CONTROL),
    ] {
        let mut app = load();
        app.pane = Pane::Files;
        let mut input = InputRouter::default();
        input.dispatch_key(
            &mut app,
            KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
            20,
        );
        input.dispatch_paste(&mut app, "auth_check", 20);
        input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);

        assert_eq!(
            input.dispatch_key(&mut app, clear, 20),
            DispatchResult::Applied(Action::ClearFind)
        );
        assert!(app.file_filter.is_none());
        assert!(!app.should_quit);

        assert_eq!(
            input.dispatch_key(&mut app, clear, 20),
            DispatchResult::Applied(Action::Quit)
        );
        assert!(app.should_quit);
    }
}

#[test]
fn comment_jump_crosses_files_and_skips_resolved_threads() {
    let mut app = load();
    app.selected_file = 0;
    app.cursor = 0;
    app.pane = Pane::Files;

    let unresolved = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();

    press(&mut app, "}");

    assert_eq!(app.pane, Pane::Diff);
    assert_eq!(app.files[app.selected_file].path, unresolved.path);
    assert_eq!(app.focused_thread.as_deref(), Some(unresolved.id.as_str()));
    assert!(
        unresolved.anchors_to(&app.files[app.selected_file].lines[app.cursor])
    );
}

#[test]
fn comment_jump_reports_when_none_remain() {
    let mut app = load();
    app.selected_file = 0;
    app.cursor = 0;

    press(&mut app, "}");
    let landed = app.selected_file;

    press(&mut app, "}");

    assert_eq!(app.selected_file, landed);
    assert_eq!(app.status, "no more comments");
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

    let template = app.pr.as_ref().unwrap().threads[0].clone();
    let threads: Vec<prtui::model::ReviewThread> = rows
        .iter()
        .enumerate()
        .map(|(index, &row)| prtui::model::ReviewThread {
            id: format!("thread-{index}"),
            path: file.path.clone(),
            line: file.lines[row].new_line,
            original_line: None,
            is_resolved: false,
            is_outdated: false,
            ..template.clone()
        })
        .collect();
    app.threads_by_path.insert(file.path, threads);

    app.cursor = 0;
    app.focused_thread = None;

    for &row in &rows {
        press(&mut app, "}");
        assert_eq!(app.cursor, row);
    }
    assert_eq!(app.focused_thread.as_deref(), Some("thread-2"));

    for &row in rows.iter().rev().skip(1) {
        press(&mut app, "{");
        assert_eq!(app.cursor, row);
    }
    assert_eq!(app.focused_thread.as_deref(), Some("thread-0"));
}

fn search_for(app: &mut App, query: &str) -> InputRouter {
    let mut input = InputRouter::default();

    app.pane = Pane::Diff;
    input.dispatch_key(
        app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
        20,
    );
    input.dispatch_paste(app, query, 20);

    input
}

#[test]
fn slash_filters_the_tree_from_the_files_pane_and_searches_from_the_diff() {
    let mut app = load();
    let mut input = InputRouter::default();

    app.pane = Pane::Files;
    input.dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
        20,
    );
    assert_eq!(app.mode, Mode::Filter);
    input.dispatch_key(&mut app, KeyCode::Escape.into(), 20);

    app.pane = Pane::Diff;
    input.dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
        20,
    );
    assert_eq!(app.mode, Mode::Search);
    assert!(app.file_filter.is_none());
}

#[test]
fn n_cycles_every_match_and_wraps_at_the_end() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);

    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(
        app.search_summary().0,
        1,
        "accepting lands on the first match"
    );

    let total = app.search_matches().len();
    assert!(total > 1, "a one-match needle cannot exercise wrapping");

    let mut visited = vec![app.cursor];
    for _ in 1..total {
        press(&mut app, "n");
        visited.push(app.cursor);
    }
    assert_eq!(app.search_summary().0, total, "n reaches the last match");

    let mut distinct = visited.clone();
    distinct.dedup();
    assert_eq!(distinct, visited, "each press advances to a new row");

    press(&mut app, "n");
    assert_eq!(app.cursor, visited[0], "the last match wraps to the first");
    assert_eq!(app.search_summary().0, 1);

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
    assert_eq!(app.search_summary().0, 1);
}

#[test]
fn search_matches_comment_bodies_and_focuses_the_thread() {
    let mut app = load();
    let thread = park_on_unresolved_thread(&mut app);
    app.cursor = 0;
    app.focused_thread = None;

    let word = thread.comments[0]
        .body
        .split_whitespace()
        .find(|word| word.len() > 5 && word.chars().all(char::is_alphanumeric))
        .expect("fixture comment has a searchable word")
        .to_string();

    let mut input = search_for(&mut app, &word);
    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);

    let matches = app.search_matches();
    assert!(
        matches
            .iter()
            .any(|hit| hit.thread_id() == Some(thread.id.as_str())),
        "the comment body should match"
    );

    for _ in 0..matches.len() {
        if app.focused_thread.as_deref() == Some(thread.id.as_str()) {
            break;
        }
        press(&mut app, "n");
    }

    assert_eq!(app.focused_thread.as_deref(), Some(thread.id.as_str()));
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

    input.dispatch_key(&mut app, KeyCode::Escape.into(), 20);

    assert_eq!(app.mode, Mode::Normal);
    assert_eq!(app.cursor, 3);
    assert_eq!(app.diff_scroll, 1);
    assert!(app.search.is_none());
}

#[test]
fn smartcase_ignores_case_until_the_query_carries_some() {
    use prtui::app::search;

    assert!(search::is_match("Total Timeout", "timeout"));
    assert!(search::is_match("Total Timeout", "Timeout"));
    assert!(!search::is_match("Total Timeout", "TIMEOUT"));
    assert!(!search::is_match("total timeout", "Timeout"));

    assert_eq!(search::ranges("ab AB ab", "ab"), vec![0..2, 3..5, 6..8]);
    assert_eq!(search::ranges("ab AB ab", "AB"), vec![3..5]);
    assert!(search::ranges("anything", "").is_empty());
}

#[test]
fn escape_clears_a_committed_search_before_it_quits() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);
    assert!(app.search.is_some());

    let mut input = InputRouter::default();
    input.dispatch_key(&mut app, KeyCode::Escape.into(), 20);
    assert!(app.search.is_none());
    assert!(!app.should_quit, "the first escape only clears the pattern");

    input.dispatch_key(&mut app, KeyCode::Escape.into(), 20);
    assert!(app.should_quit);
}

#[test]
fn match_and_comment_motions_are_normal_mode_only() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);

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
    let half = 10;

    input.dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('d'), Modifiers::CONTROL),
        half * 2,
    );
    assert!(
        (1..=half).contains(&app.cursor),
        "ctrl-d advances at most half a viewport, landed on {}",
        app.cursor
    );

    input.dispatch_key(
        &mut app,
        KeyEvent::new(KeyCode::Char('u'), Modifiers::CONTROL),
        half * 2,
    );
    assert_eq!(app.cursor, 0, "ctrl-u walks the same distance back");
}

#[test]
fn match_motions_find_the_nearest_hit_from_an_unmatched_row() {
    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    input.dispatch_key(&mut app, KeyCode::Enter.into(), 20);

    let rows: Vec<usize> =
        app.search_matches().iter().map(Match::row).collect();
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
    app.focused_thread = None;
    press(&mut app, "{");
    assert_eq!(app.cursor, row, "{{ reaches back to the comment above");
    assert_eq!(app.focused_thread.as_deref(), Some(thread.id.as_str()));

    app.cursor = row - 1;
    app.focused_thread = None;
    press(&mut app, "}");
    assert_eq!(app.cursor, row, "}} reaches forward to the comment below");
    assert_eq!(app.focused_thread.as_deref(), Some(thread.id.as_str()));
}

#[test]
fn both_prompts_step_with_arrows_and_control_keys() {
    let ctrl = |c| KeyEvent::new(KeyCode::Char(c), Modifiers::CONTROL);

    let mut app = load();
    app.pane = Pane::Files;
    let mut input = InputRouter::default();

    press(&mut app, "/auth_check");
    assert_eq!(app.filtered_file_indices(), vec![2, 3]);

    for (key, expected) in [
        (KeyEvent::from(KeyCode::Up), 2),
        (KeyEvent::from(KeyCode::Down), 3),
        (ctrl('p'), 2),
        (ctrl('n'), 3),
    ] {
        input.dispatch_key(&mut app, key, 20);
        assert_eq!(app.selected_file, expected, "{key:?} steps the filter");
    }

    input.dispatch_key(&mut app, ctrl('['), 20);
    assert_eq!(app.mode, Mode::Normal, "ctrl-[ cancels the filter prompt");
    assert!(app.file_filter.is_none());

    let mut app = load();
    let mut input = search_for(&mut app, "cobra");
    let rows: Vec<usize> =
        app.search_matches().iter().map(Match::row).collect();

    for (key, expected) in [
        (KeyEvent::from(KeyCode::Down), rows[1]),
        (KeyEvent::from(KeyCode::Up), rows[0]),
        (ctrl('n'), rows[1]),
        (ctrl('p'), rows[0]),
    ] {
        input.dispatch_key(&mut app, key, 20);
        assert_eq!(app.cursor, expected, "{key:?} steps the search prompt");
    }

    input.dispatch_key(&mut app, ctrl('['), 20);
    assert_eq!(app.mode, Mode::Normal, "ctrl-[ cancels the search prompt");
    assert!(app.search.is_none());
}
