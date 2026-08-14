use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use prtui::app::action::Action;
use prtui::app::input::InputRouter;
use prtui::app::{App, Pane};
use prtui::model::{parse_files, parse_meta, LineKind};
use prtui::ui;
use ratatui::backend::TestBackend;
use ratatui::Terminal;

fn load() -> App {
    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    let meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();

    let mut app = App::new();
    app.files = parse_files(&files).unwrap();
    app.set_meta(parse_meta(&meta).unwrap());
    app
}

#[test]
fn parses_files_and_threads() {
    let app = load();

    assert_eq!(app.files.len(), 4);
    assert_eq!(app.pr.as_ref().unwrap().number, 9000);
    assert_eq!(app.pr.as_ref().unwrap().threads.len(), 2);
}

#[test]
fn hunk_line_numbers_advance_correctly() {
    let app = load();
    let file = app.files.iter().find(|f| !f.lines.is_empty()).unwrap();

    for line in &file.lines {
        match line.kind {
            LineKind::Added => assert!(line.old_line.is_none() && line.new_line.is_some()),
            LineKind::Removed => assert!(line.old_line.is_some() && line.new_line.is_none()),
            LineKind::Context => assert!(line.old_line.is_some() && line.new_line.is_some()),
            LineKind::Hunk => assert!(line.old_line.is_none() && line.new_line.is_none()),
        }
    }

    let added: Vec<u32> = file
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .filter_map(|l| l.new_line)
        .collect();

    assert!(added.windows(2).all(|w| w[0] < w[1]), "new-side numbers must increase");
}

#[test]
fn renders_header_and_diff() {
    let mut app = load();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app, "")).unwrap();

    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("#9000"), "header should show the PR number");
    assert!(rendered.contains("NORMAL"), "status bar should show the mode");
    assert!(rendered.contains("files"), "status bar should show the active pane");
    assert!(
        app.files.iter().any(|f| rendered.contains(f.path.split('/').next_back().unwrap())),
        "file list should show at least one changed file"
    );
}

#[test]
fn pending_input_is_visible_in_the_status_line() {
    let mut app = load();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app, "123456")).unwrap();

    assert!(terminal.backend().to_string().contains("123456"));
}

/// Drives the app the way the event loop does: raw keys through the keymap.
fn press(app: &mut App, keys: &str) {
    let mut input = InputRouter::default();
    for c in keys.chars() {
        let key = KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE);
        input.dispatch_key(app, key, 20);
    }
}

#[test]
fn scrolling_stays_in_bounds() {
    let mut app = load();
    app.pane = Pane::Diff;

    for _ in 0..10_000 {
        press(&mut app, "j");
    }
    assert!(app.diff_scroll <= app.diff_len());
    assert!(app.cursor < app.diff_len());

    for _ in 0..10_000 {
        press(&mut app, "k");
    }
    assert_eq!(app.diff_scroll, 0);
    assert_eq!(app.cursor, 0);
}

#[test]
fn renders_large_diff_in_constant_time() {
    let mut app = load();
    let file = app.files.iter().position(|f| f.lines.len() > 5).unwrap();
    app.selected_file = file;

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();

    let started = std::time::Instant::now();
    for _ in 0..500 {
        terminal.draw(|frame| ui::draw(frame, &mut app, "")).unwrap();
    }
    let elapsed = started.elapsed();

    assert!(elapsed.as_millis() < 1000, "500 frames took {elapsed:?}");
}

#[test]
fn word_diff_marks_only_changed_tokens() {
    use prtui::highlight::highlight_file;
    use prtui::model::DiffLine;

    let line = |kind, text: &str| DiffLine {
        kind,
        text: text.into(),
        old_line: Some(1),
        new_line: Some(1),
    };

    let lines = vec![
        line(LineKind::Removed, "let total = compute(alpha, 10);"),
        line(LineKind::Added, "let total = compute(beta, 20);"),
    ];

    let styled = highlight_file("x.rs", &lines);

    let marked = |row: &Vec<prtui::highlight::Segment>| -> Vec<String> {
        row.iter().filter(|s| s.is_emphasis).map(|s| s.text.clone()).collect()
    };

    // Whitespace-only tokenization used to flag `compute(alpha,` wholesale.
    assert_eq!(marked(&styled[0]), vec!["alpha", "10"]);
    assert_eq!(marked(&styled[1]), vec!["beta", "20"]);
}

#[test]
fn hiding_the_tree_forces_focus_to_the_diff() {
    let mut app = load();
    assert!(app.is_files_visible);
    assert_eq!(app.pane, Pane::Files);

    press(&mut app, "f");
    assert!(!app.is_files_visible);
    assert_eq!(app.pane, Pane::Diff);

    // Tab must not hand focus back to a pane that is not on screen.
    app.apply(Action::TogglePane, 20);
    assert_eq!(app.pane, Pane::Diff);

    press(&mut app, "f");
    assert!(app.is_files_visible);
}

#[test]
fn visual_selection_paints_every_row_in_the_span() {
    use prtui::app::mode::Selection;

    let mut app = load();
    app.pane = Pane::Diff;
    app.selected_file = app
        .files
        .iter()
        .enumerate()
        .max_by_key(|(_, f)| f.lines.len())
        .map(|(i, _)| i)
        .unwrap();
    app.cursor = 4;

    let snapshot = |app: &mut App| -> Vec<Option<ratatui::style::Color>> {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal.draw(|frame| ui::draw(frame, app, "")).unwrap();
        let buffer = terminal.backend().buffer();
        (0..30).map(|row| buffer.cell((45, row)).unwrap().style().bg).collect()
    };

    let plain = snapshot(&mut app);
    app.selection = Some(Selection { anchor: 2, head: 6 });
    let selected = snapshot(&mut app);

    // Diff row N draws on screen row N+1; the header occupies row 0.
    for row in 3..=7 {
        assert_ne!(
            plain[row], selected[row],
            "diff row {} is inside the selection and must change tint",
            row - 1
        );
    }

    for row in [2, 8, 9] {
        assert_eq!(
            plain[row], selected[row],
            "diff row {} is outside the selection and must not change",
            row - 1
        );
    }
}

#[test]
fn every_selected_row_carries_the_left_bar() {
    use prtui::app::mode::Selection;

    let mut app = load();
    app.pane = Pane::Diff;
    app.selection = Some(Selection { anchor: 1, head: 4 });

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app, "")).unwrap();

    let buffer = terminal.backend().buffer();
    // The tree is 30 columns at this width, so the diff's own gutter starts there.
    let bar_column = 30;
    for row in 2..=5 {
        assert_eq!(
            buffer.cell((bar_column, row)).unwrap().symbol(),
            "▍",
            "selected row {} should show the selection bar",
            row - 1
        );
    }
}

#[test]
fn the_composer_opens_over_the_diff_with_its_anchor_in_the_title() {
    let mut app = load();
    app.pane = Pane::Diff;
    app.cursor = app.files[app.selected_file]
        .lines
        .iter()
        .position(|l| l.kind != LineKind::Hunk)
        .unwrap();

    app.apply(Action::StartComment, 20);
    assert!(app.composer.is_some());

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app, "")).unwrap();

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("comment ·"), "composer should be titled");
    assert!(rendered.contains("INSERT"), "status bar should show insert mode");
}

#[test]
fn a_saved_draft_marks_its_lines_in_the_gutter() {
    let mut app = load();
    app.pane = Pane::Diff;
    app.cursor = app.files[app.selected_file]
        .lines
        .iter()
        .position(|l| l.kind != LineKind::Hunk)
        .unwrap();

    app.apply(Action::StartComment, 20);
    app.composer.as_mut().unwrap().editor.lines = edtui::Lines::from("needs a guard");
    app.apply(Action::CommitComment, 20);
    assert_eq!(app.drafts.len(), 1);

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &mut app, "")).unwrap();

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains('✎'), "draft lines carry a pencil marker");
}
