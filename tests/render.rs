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
    let app = load();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| ui::draw(frame, &app)).unwrap();

    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("#9000"), "header should show the PR number");
    assert!(rendered.contains("FILES"), "status bar should show the active pane");
    assert!(
        app.files.iter().any(|f| rendered.contains(f.path.split('/').next_back().unwrap())),
        "file list should show at least one changed file"
    );
}

#[test]
fn scrolling_stays_in_bounds() {
    let mut app = load();
    app.pane = Pane::Diff;

    for _ in 0..10_000 {
        app.on_key('j', 20);
    }
    assert!(app.diff_scroll <= app.diff_len());

    for _ in 0..10_000 {
        app.on_key('k', 20);
    }
    assert_eq!(app.diff_scroll, 0);
}

#[test]
fn renders_large_diff_in_constant_time() {
    let mut app = load();
    let file = app.files.iter().position(|f| f.lines.len() > 5).unwrap();
    app.selected_file = file;

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();

    let started = std::time::Instant::now();
    for _ in 0..500 {
        terminal.draw(|frame| ui::draw(frame, &app)).unwrap();
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

    app.on_key('f', 20);
    assert!(!app.is_files_visible);
    assert_eq!(app.pane, Pane::Diff);

    // Tab must not hand focus back to a pane that is not on screen.
    app.on_key('\t', 20);
    assert_eq!(app.pane, Pane::Diff);

    app.on_key('f', 20);
    assert!(app.is_files_visible);
}
