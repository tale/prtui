use prtui::app::action::Action;
use prtui::app::input::DispatchResult;
use prtui::app::input::InputRouter;
use prtui::app::{App, Pane};
use prtui::images::Placement;
use prtui::images::{self, CellSize, Images, Support};
use prtui::layout::Layout;
use prtui::model::{LineKind, Side, parse_files, parse_meta};
use prtui::renderer::ThemeMode;
use prtui::ui;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
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

/// How far the open conversation can scroll in a terminal of this size.
fn thread_limit(app: &App, width: u16, height: u16) -> usize {
    Layout::compute(Rect::new(0, 0, width, height), app)
        .rows
        .body_limit()
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

fn summary(app: &App) -> (usize, usize) {
    app.search_summary(&layout_of(app))
}

/// Draws a frame and returns the escape sequences its images would be painted
/// with, which is the pairing the event loop performs.
fn paint_images(terminal: &mut Terminal<TestBackend>, app: &mut App) -> String {
    let mut placements = Vec::new();
    terminal
        .draw(|frame| placements = paint(frame, app))
        .unwrap();

    app.images.frame_commands(&placements)
}

/// Renders one frame through the path the event loop uses: layout first, then
/// draw against it.
fn paint(frame: &mut Frame, app: &App) -> Vec<Placement> {
    let layout = Layout::compute(frame.area(), app);
    ui::draw(frame, app, &layout, "")
}

/// Drives the app the way the event loop does: raw keys through the keymap.
fn press(app: &mut App, keys: &str) {
    let input = &mut InputRouter::default();
    for c in keys.chars() {
        send(input, app, KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
    }
}

/// Renders a frame at the size most tests use and returns it as text.
fn draw(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, app);
        })
        .unwrap();

    terminal.backend().to_string()
}

/// Body text long enough to overflow any thread viewport.
fn paragraphs(count: usize, label: &str) -> String {
    (1..=count).fold(String::new(), |mut body, index| {
        let _ = writeln!(body, "{label} {index}\n");
        body
    })
}

fn load() -> App {
    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    let meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();

    let mut app = App::new();
    app.set_files(parse_files(&files).unwrap());
    app.set_meta(parse_meta(&meta).unwrap());
    app
}

#[test]
fn parses_files_and_threads() {
    let app = load();

    assert_eq!(app.files.len(), 4);
    assert_eq!(app.pr.as_ref().unwrap().number, 9000);
    assert_eq!(app.pr.as_ref().unwrap().threads.len(), 2);

    let thread = &app.pr.as_ref().unwrap().threads[0];
    assert_eq!(thread.id, "PRRT_kwDODKw3uc48Rk4m");
    assert_eq!(thread.side, Side::Right);
    assert_eq!(thread.line, Some(130));
    assert_eq!(thread.original_line, Some(130));
    assert_eq!(thread.comments[0].created_at, "2024-04-29T14:06:54Z");
}

fn show_thread(app: &mut App, thread: &prtui::model::ReviewThread) {
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
    app.diff_scroll = app.cursor.saturating_sub(3);
    app.pane = Pane::Diff;
    app.is_files_visible = false;
}

#[test]
fn renders_unresolved_thread_summary_inline() {
    let mut app = load();
    let mut thread = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();
    let mut reply = thread.comments[0].clone();
    reply.id = "reply".into();
    reply.author = "andyfeller".into();
    reply.body = "A reply inside the same review thread.".into();
    reply.created_at = "2024-04-29T15:01:00Z".into();
    thread.comments.push(reply);
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("Lol not suspicious of coupling at all."));
    assert!(rendered.contains("@williammartin"));
    assert!(rendered.contains("1 reply · open"));
}

#[test]
fn collapsed_thread_summary_uses_rendered_gfm_text() {
    let mut app = load();
    let mut thread = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();
    thread.comments[0].body =
        "_Minor_ and **important** with `inline code`".into();
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);

    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    assert!(
        terminal
            .backend()
            .to_string()
            .contains("Minor and important with inline code")
    );
}

#[test]
fn focused_thread_expands_into_its_full_conversation() {
    let mut app = load();
    let mut thread = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();
    let mut reply = thread.comments[0].clone();
    reply.id = "reply".into();
    reply.author = "andyfeller".into();
    reply.body = "This is the full reply body.".into();
    thread.comments.push(reply);
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);
    app.focused_thread = Some(thread.id.clone());
    app.expanded_thread = Some(thread.id.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();
    let focused_row = rendered
        .lines()
        .find(|line| line.contains("2 comments"))
        .expect("expanded thread summary should be visible");

    assert!(focused_row.contains('▍'));
    assert!(focused_row.contains("◆ ▾ 2 comments · open"));
    assert!(rendered.contains("Lol not suspicious of coupling at all."));
    assert!(rendered.contains("This is the full reply body."));
    assert!(rendered.contains("reply 1/1"));
    assert!(rendered.contains("collapse"));
}

#[test]
fn expanded_thread_can_scroll_to_its_complete_conversation() {
    let mut app = load();
    let mut thread = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();
    thread.comments[0].body =
        paragraphs(18, "Paragraph") + "TAIL CONTENT IS REACHABLE";
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);
    app.focused_thread = Some(thread.id.clone());
    app.expanded_thread = Some(thread.id.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let first_page = terminal.backend().to_string();

    let small_viewport_limit = thread_limit(&app, 100, 24);
    assert!(small_viewport_limit > 0);
    assert!(first_page.contains("↓"));
    assert!(first_page.contains("more"));

    app.thread_scroll = small_viewport_limit;
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let last_page = terminal.backend().to_string();

    assert!(last_page.contains("↑"));
    assert!(last_page.contains("earlier"));
    assert!(last_page.contains("TAIL CONTENT IS REACHABLE"));
    assert!(last_page.contains("j/k scroll"));

    app.thread_scroll = 0;
    let mut large_terminal = Terminal::new(TestBackend::new(100, 40)).unwrap();
    large_terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    // A taller terminal gives the conversation more room, so less of it is
    // left to scroll through.
    assert!(thread_limit(&app, 100, 40) < small_viewport_limit);
    assert!(
        large_terminal
            .backend()
            .to_string()
            .contains("Paragraph 10")
    );
}

#[test]
fn long_reply_keeps_its_identity_visible_while_scrolling() {
    let mut app = load();
    let mut thread = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();
    thread.comments[0].body = "Opening comment.".into();
    let mut reply = thread.comments[0].clone();
    reply.id = "long-reply".into();
    reply.author = "andyfeller".into();
    reply.body = paragraphs(24, "Reply paragraph");
    thread.comments.push(reply);
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);
    app.focused_thread = Some(thread.id.clone());
    app.expanded_thread = Some(thread.id.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    assert!(thread_limit(&app, 100, 24) > 5);

    app.thread_scroll = 5;
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("↳ @andyfeller"));
    assert!(rendered.contains("reply 1/1 · continued"));
    assert!(rendered.contains("↑ 5 earlier"));
}

#[test]
fn multiple_threads_on_one_line_render_as_one_group() {
    let mut app = load();
    let base = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();
    let mut threads = Vec::new();
    for index in 1..=4 {
        let mut thread = base.clone();
        thread.id = format!("thread-{index}");
        thread.comments[0].body = format!("Discussion number {index}");
        threads.push(thread);
    }
    app.threads_by_path.insert(base.path, threads.clone());
    show_thread(&mut app, &threads[0]);

    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("◆ 4 open threads"));
    assert!(rendered.contains("@williammartin  Discussion number 1"));
    assert!(rendered.contains("@williammartin  Discussion number 4"));

    app.focused_thread = Some(threads[3].id.clone());
    app.expanded_thread = Some(threads[3].id.clone());
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let expanded = terminal.backend().to_string();

    assert!(expanded.contains("… 3 earlier"));
    assert!(expanded.contains("Discussion number 4"));
}

#[test]
fn resolved_threads_render_as_compact_rows() {
    let mut app = load();
    let thread = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| thread.is_resolved)
        .unwrap()
        .clone();
    show_thread(&mut app, &thread);

    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("◇ @williammartin"));
    assert!(rendered.contains("Could I interest you in the following tests:"));
    assert!(rendered.contains("· resolved"));
}

#[test]
fn left_side_threads_anchor_to_removed_lines() {
    let mut app = load();
    let (file_index, line_index, old_line) = app
        .files
        .iter()
        .enumerate()
        .find_map(|(file_index, file)| {
            file.lines
                .iter()
                .enumerate()
                .find_map(|(line_index, line)| {
                    (line.kind == LineKind::Removed).then(|| {
                        (file_index, line_index, line.old_line.unwrap())
                    })
                })
        })
        .unwrap();
    let path = app.files[file_index].path.clone();
    let mut thread = app.pr.as_ref().unwrap().threads[1].clone();
    thread.path = path.clone();
    thread.line = Some(old_line);
    thread.original_line = Some(old_line);
    thread.side = Side::Left;
    thread.comments[0].body = "This belongs to the removed side.".into();
    app.threads_by_path.clear();
    app.threads_by_path.insert(path, vec![thread]);
    app.selected_file = file_index;
    app.cursor = line_index;
    app.diff_scroll = line_index.saturating_sub(2);
    app.pane = Pane::Diff;
    app.is_files_visible = false;

    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    assert!(
        terminal
            .backend()
            .to_string()
            .contains("This belongs to the removed side.")
    );
}

#[test]
fn outdated_threads_render_compactly_after_the_diff() {
    let mut app = load();
    let file_index = app
        .files
        .iter()
        .position(|file| !file.lines.is_empty())
        .unwrap();
    let path = app.files[file_index].path.clone();
    let mut thread = app.pr.as_ref().unwrap().threads[1].clone();
    thread.path = path.clone();
    thread.line = None;
    thread.original_line = Some(999_999);
    thread.is_outdated = true;
    thread.comments[0].body = "Discussion from an earlier diff.".into();
    app.threads_by_path.clear();
    app.threads_by_path.insert(path, vec![thread]);
    app.selected_file = file_index;
    app.cursor = app.files[file_index].lines.len() - 1;
    app.diff_scroll = app.cursor.saturating_sub(3);
    app.pane = Pane::Diff;
    app.is_files_visible = false;

    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("◇ @williammartin"));
    assert!(rendered.contains("Discussion from an earlier diff."));
    assert!(rendered.contains("· outdated"));
}

#[test]
fn hunk_line_numbers_advance_correctly() {
    let app = load();
    let file = app.files.iter().find(|f| !f.lines.is_empty()).unwrap();

    for line in &file.lines {
        match line.kind {
            LineKind::Added => {
                assert!(line.old_line.is_none() && line.new_line.is_some());
            }
            LineKind::Removed => {
                assert!(line.old_line.is_some() && line.new_line.is_none());
            }
            LineKind::Context => {
                assert!(line.old_line.is_some() && line.new_line.is_some());
            }
            LineKind::Hunk => {
                assert!(line.old_line.is_none() && line.new_line.is_none());
            }
        }
    }

    let added: Vec<u32> = file
        .lines
        .iter()
        .filter(|l| l.kind == LineKind::Added)
        .filter_map(|l| l.new_line)
        .collect();

    assert!(
        added.windows(2).all(|w| w[0] < w[1]),
        "new-side numbers must increase"
    );
}

#[test]
fn renders_header_and_diff() {
    let app = load();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    let rendered = terminal.backend().to_string();

    assert!(
        rendered.contains("#9000"),
        "header should show the PR number"
    );
    assert!(
        rendered.contains("NORMAL"),
        "status bar should show the mode"
    );
    assert!(
        rendered.contains("files"),
        "status bar should show the active pane"
    );
    assert!(
        app.files
            .iter()
            .any(|f| rendered.contains(f.path.split('/').next_back().unwrap())),
        "file list should show at least one changed file"
    );
}

#[test]
fn multi_digit_thread_badge_keeps_diff_counts_visible() {
    let mut app = load();
    let path = app.files[0].path.clone();
    let thread = app.pr.as_ref().unwrap().threads[1].clone();
    app.threads_by_path.insert(path, vec![thread; 10]);

    let mut terminal = Terminal::new(TestBackend::new(52, 12)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    let file_row = rendered
        .lines()
        .find(|line| line.contains("◆ 10"))
        .expect("the complete thread count should be visible");
    assert!(file_row.contains("+1"));
    assert!(file_row.contains("-0"));
}

#[test]
fn loading_state_is_centered_once_and_animates() {
    let mut app = App::new();
    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();

    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let first = terminal.backend().to_string();
    assert_eq!(first.matches("loading changes").count(), 1);
    assert!(first.contains('⠋'));

    app.advance_loading();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let second = terminal.backend().to_string();
    assert!(second.contains('⠙'));
    assert_ne!(first, second, "the loading indicator should advance frames");
}

#[test]
fn files_release_the_loading_gate_without_metadata() {
    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    let mut app = App::new();
    app.set_files(parse_files(&files).unwrap());

    let mut terminal = Terminal::new(TestBackend::new(80, 20)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(!rendered.contains("loading changes"));
    assert!(rendered.contains("verify.go"));
}

#[test]
fn metadata_can_arrive_while_the_file_loader_continues() {
    let meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();
    let mut app = App::new();
    app.set_meta(parse_meta(&meta).unwrap());

    let mut terminal = Terminal::new(TestBackend::new(100, 20)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("#9000"));
    assert!(rendered.contains("loading changes"));
}

#[test]
fn bottom_bar_shows_the_focused_pane_s_actions() {
    let app = load();
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("j/k move"));
    assert!(rendered.contains("↵ open"));
}

#[test]
fn the_filter_prompt_renders_while_typing_and_stays_after_commit() {
    let mut app = load();
    press(&mut app, "/auth_check_test");

    let typing = draw(&app);
    assert!(typing.contains("FILTER"));
    assert!(typing.contains("/auth_check_test"));
    assert!(typing.contains("1/1 matches"));
    // The sidebar deliberately elides the left edge of long basenames.
    assert!(typing.contains("heck_test.go"));
    assert!(
        !typing.contains("verify_test.go"),
        "non-matching paths leave the tree"
    );

    let mut input = InputRouter::default();
    send(&mut input, &mut app, KeyCode::Enter.into());

    let committed = draw(&app);
    assert!(committed.contains("NORMAL"));
    assert!(committed.contains("/auth_check_test"));
    assert!(committed.contains("edit filter"));
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
fn word_diff_marks_only_changed_tokens() {
    use prtui::model::DiffLine;
    use prtui::renderer::Renderer;

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

    let styled = Renderer::default().highlight_file("x.rs", &lines);

    let marked =
        |row: &Vec<prtui::renderer::Segment>, source: &str| -> Vec<String> {
            row.iter()
                .filter(|segment| segment.is_emphasis)
                .map(|segment| source[segment.range.clone()].to_string())
                .collect()
        };

    // Whitespace-only tokenization used to flag `compute(alpha,` wholesale.
    assert_eq!(marked(&styled[0], &lines[0].text), vec!["alpha", "10"]);
    assert_eq!(marked(&styled[1], &lines[1].text), vec!["beta", "20"]);
}

#[test]
fn light_mode_uses_light_diff_and_syntax_palettes() {
    use prtui::renderer::{Renderer, Theme, ThemeMode};

    let mut app = App::with_renderer(Renderer::new(ThemeMode::Light));
    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    app.set_files(parse_files(&files).unwrap());
    app.is_files_visible = false;
    app.ensure_highlighted();

    let added = app
        .current_file()
        .unwrap()
        .lines
        .iter()
        .position(|line| line.kind == LineKind::Added)
        .unwrap();
    let mut terminal = Terminal::new(TestBackend::new(100, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((0, (added + 2) as u16))
            .unwrap()
            .style()
            .bg,
        Some(Theme::light().add),
    );

    let colors = app.highlighted().unwrap()[added]
        .iter()
        .map(|segment| segment.color)
        .collect::<Vec<_>>();
    assert!(
        colors
            .iter()
            .all(|&(r, g, b)| u16::from(r) + u16::from(g) + u16::from(b) < 600),
        "light-mode syntax must use dark foregrounds: {colors:?}",
    );
}

#[test]
fn switching_terminal_appearance_invalidates_old_syntax_colors() {
    let mut app = load();
    app.ensure_highlighted();
    assert!(app.highlighted().is_some());

    assert!(app.set_theme_mode(ThemeMode::Light));
    assert!(app.highlighted().is_none());
    assert!(!app.set_theme_mode(ThemeMode::Light));
}

#[test]
fn hiding_the_tree_forces_focus_to_the_diff() {
    let mut app = load();
    assert!(app.is_files_visible);
    assert_eq!(app.pane, Pane::Files);

    press(&mut app, "f");
    assert!(!app.is_files_visible);
    assert_eq!(app.pane, Pane::Diff);

    // Tab is also the recovery path: it reopens and focuses the tree.
    act(&mut app, &Action::TogglePane);
    assert!(app.is_files_visible);
    assert_eq!(app.pane, Pane::Files);

    press(&mut app, "f");
    assert!(!app.is_files_visible);
    assert_eq!(app.pane, Pane::Diff);
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
        terminal
            .draw(|frame| {
                paint(frame, app);
            })
            .unwrap();
        let buffer = terminal.backend().buffer();
        (0..30)
            .map(|row| buffer.cell((45, row)).unwrap().style().bg)
            .collect()
    };

    let plain = snapshot(&mut app);
    app.selection = Some(Selection { anchor: 2, head: 6 });
    let selected = snapshot(&mut app);

    // Diff row N draws on screen row N+2; the header and pane title occupy two rows.
    for row in 4..=8 {
        assert_ne!(
            plain[row],
            selected[row],
            "diff row {} is inside the selection and must change tint",
            row - 2
        );
    }

    for row in [3, 9, 10] {
        assert_eq!(
            plain[row],
            selected[row],
            "diff row {} is outside the selection and must not change",
            row - 2
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

    act(&mut app, &Action::StartComment);
    assert!(app.composer.is_some());

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains("comment ·"), "composer should be titled");
    assert!(
        rendered.contains("INSERT"),
        "status bar should show insert mode"
    );
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

    act(&mut app, &Action::StartComment);
    app.composer
        .as_mut()
        .unwrap()
        .editor
        .set_text("needs a guard");
    act(&mut app, &Action::CommitComment);
    assert_eq!(app.drafts.len(), 1);

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    let rendered = terminal.backend().to_string();
    assert!(rendered.contains('✎'), "draft lines carry a pencil marker");
}

const IMAGE_URL: &str = "https://github.com/user-attachments/assets/shot.png";

fn thread_with_image(app: &mut App) -> prtui::model::ReviewThread {
    let mut thread = app
        .pr
        .as_ref()
        .unwrap()
        .threads
        .iter()
        .find(|thread| !thread.is_resolved)
        .unwrap()
        .clone();
    thread.comments[0].body =
        format!("Before and after:\n\n![shot]({IMAGE_URL})");
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    thread
}

#[test]
fn expanding_a_thread_queues_its_images() {
    let mut app = load();
    let thread = thread_with_image(&mut app);
    show_thread(&mut app, &thread);
    app.images = Images::new(Support::Enabled);
    app.focused_thread = Some(thread.id.clone());

    act(&mut app, &Action::Activate);

    assert_eq!(app.images.take_pending(), vec![IMAGE_URL.to_string()]);
}

#[test]
fn a_loaded_image_is_drawn_over_the_rows_it_reserves() {
    let mut app = load();
    let thread = thread_with_image(&mut app);
    show_thread(&mut app, &thread);
    app.images = Images::new(Support::Enabled);
    app.images.set_cell_size(Some(CellSize {
        width: 8,
        height: 16,
    }));
    app.images.insert(
        IMAGE_URL.into(),
        images::decode(include_bytes!("fixtures/screenshot.png"))
            .map_err(|err| err.to_string()),
    );
    app.focused_thread = Some(thread.id.clone());
    app.expanded_thread = Some(thread.id.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let commands = paint_images(&mut terminal, &mut app);

    // 80x80 pixels over 8x16 cells is 10 columns and 5 rows.
    assert!(commands.contains("\x1b_Ga=t,i=1,f=100,t=d,q=2,"));
    assert!(
        commands.contains("a=p,i=1,p=1,c=10,r=5,x=0,y=0,w=80,h=80,C=1,q=2"),
        "expected a full-height placement, got {commands:?}"
    );
    assert!(!terminal.backend().to_string().contains("▭ shot"));
}

#[test]
fn scrolling_past_an_image_crops_it_to_what_is_visible() {
    let mut app = load();
    let mut thread = thread_with_image(&mut app);
    thread.comments[0].body =
        format!("![shot]({IMAGE_URL})\n\n") + &paragraphs(20, "Paragraph");
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);
    app.images = Images::new(Support::Enabled);
    app.images.set_cell_size(Some(CellSize {
        width: 8,
        height: 16,
    }));
    app.images.insert(
        IMAGE_URL.into(),
        images::decode(include_bytes!("fixtures/screenshot.png"))
            .map_err(|err| err.to_string()),
    );
    app.focused_thread = Some(thread.id.clone());
    app.expanded_thread = Some(thread.id.clone());
    app.thread_scroll = 3;

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let commands = paint_images(&mut terminal, &mut app);

    assert!(
        commands.contains(",r=3,x=0,y=32,w=80,h=48,"),
        "the scrolled-off rows should be cropped from the source, got {commands:?}"
    );
}

#[test]
fn terminals_without_graphics_support_keep_the_alt_text() {
    let mut app = load();
    let thread = thread_with_image(&mut app);
    show_thread(&mut app, &thread);
    app.focused_thread = Some(thread.id.clone());
    app.expanded_thread = Some(thread.id.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let commands = paint_images(&mut terminal, &mut app);

    assert!(commands.is_empty());
    assert!(terminal.backend().to_string().contains("▭ shot"));
}

#[test]
fn the_file_tree_marks_files_whose_threads_are_all_resolved() {
    let mut app = load();
    let path = app.files[0].path.clone();
    let mut thread = app.pr.as_ref().unwrap().threads[1].clone();
    thread.is_resolved = true;
    app.threads_by_path.insert(path, vec![thread; 2]);

    let mut terminal = Terminal::new(TestBackend::new(52, 12)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(
        rendered.lines().any(|line| line.contains("◇ 2")),
        "a settled conversation still marks its file, got:\n{rendered}"
    );
}

#[test]
fn an_undrawable_attachment_says_why() {
    let mut app = load();
    let thread = thread_with_image(&mut app);
    show_thread(&mut app, &thread);
    app.images = Images::new(Support::Enabled);
    app.images
        .insert(IMAGE_URL.into(), Err("video attachment".into()));
    app.focused_thread = Some(thread.id.clone());
    app.expanded_thread = Some(thread.id.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    assert!(
        rendered.contains("▭ video attachment · shot"),
        "the reason leads and the link still reads as itself, got:\n{rendered}"
    );
}

#[test]
fn a_terminal_that_failed_the_probe_says_so() {
    let mut app = load();
    let thread = thread_with_image(&mut app);
    show_thread(&mut app, &thread);
    app.images = Images::new(Support::Unsupported);
    app.focused_thread = Some(thread.id.clone());
    app.expanded_thread = Some(thread.id.clone());

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    assert!(
        terminal
            .backend()
            .to_string()
            .contains("▭ no image support · shot")
    );
}

#[test]
fn search_paints_only_the_matched_bytes_of_a_diff_line() {
    use prtui::app::input::InputRouter;
    use prtui::renderer::Theme;

    const NEEDLE: &str = "DisableAuthCheckFlag";

    let mut app = load();
    app.is_files_visible = false;
    app.pane = Pane::Diff;
    app.ensure_highlighted();

    let row = app
        .current_file()
        .unwrap()
        .lines
        .iter()
        .position(|line| line.text.contains(NEEDLE))
        .expect("fixture has a line calling DisableAuthCheckFlag");

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    paste(&mut input, &mut app, NEEDLE);
    send(&mut input, &mut app, KeyCode::Enter.into());

    assert_eq!(app.cursor, row, "search lands on the only match");
    assert_eq!(summary(&app), (1, 1));

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    let theme = Theme::dark();
    let buffer = terminal.backend().buffer();
    let painted = buffer
        .content()
        .iter()
        .filter(|cell| cell.style().bg == Some(theme.search_current))
        .count();

    assert_eq!(
        painted,
        NEEDLE.len(),
        "exactly the matched bytes carry the highlight"
    );

    let screen = terminal.backend().to_string();
    assert!(
        screen.contains(&format!("/{NEEDLE}")),
        "the query shows in the status bar"
    );
    assert!(screen.contains("1/1 matches"));
}

#[test]
fn parallel_highlighting_matches_the_serial_pass_for_every_file() {
    use prtui::model::DiffLine;
    use prtui::renderer::Renderer;
    use std::sync::Mutex;

    // More files than cores, so workers drain the queue rather than taking one
    // apiece and the index each result carries is actually exercised.
    let renderer = Renderer::default();
    let files: Vec<(String, Vec<DiffLine>)> = (0..64)
        .map(|n| {
            (
                format!("file{n}.rs"),
                vec![DiffLine {
                    kind: LineKind::Added,
                    text: format!("let total{n} = compute(alpha, {n});"),
                    old_line: None,
                    new_line: Some(1),
                }],
            )
        })
        .collect();

    let published = Mutex::new(vec![None; files.len()]);
    renderer.highlight_files_parallel(&files, |index, styled| {
        let mut slots = published.lock().unwrap();
        assert!(slots[index].is_none(), "file {index} published twice");
        slots[index] = Some(styled);
    });

    let published = published.into_inner().unwrap();
    for (index, (path, lines)) in files.iter().enumerate() {
        let styled = published[index]
            .as_ref()
            .unwrap_or_else(|| panic!("file {index} was never published"));
        assert_eq!(*styled, renderer.highlight_file(path, lines));
    }
}

#[test]
fn the_submit_overlay_shows_the_verdict_and_the_draft_count() {
    let mut app = load();
    app.pane = Pane::Diff;
    app.cursor = app.files[app.selected_file]
        .lines
        .iter()
        .position(|l| l.kind != LineKind::Hunk)
        .unwrap();

    act(&mut app, &Action::StartComment);
    app.composer
        .as_mut()
        .unwrap()
        .editor
        .set_text("needs a test");
    act(&mut app, &Action::CommitComment);

    press(&mut app, "s");
    let rendered = draw(&app);
    assert!(
        rendered.contains("submit review · 1 draft"),
        "the overlay should count what it will send:\n{rendered}"
    );
    assert!(
        rendered.contains("SUBMIT"),
        "status bar should show the mode"
    );
    for label in ["comment", "approve", "request changes"] {
        assert!(
            rendered.contains(label),
            "every verdict should be visible, missing {label}:\n{rendered}"
        );
    }
    assert!(
        rendered.contains("summary (optional for approve)"),
        "an empty summary should say what it is for:\n{rendered}"
    );

    act(&mut app, &Action::CycleEvent(1));
    app.submission.as_mut().unwrap().editor.set_text("ship it");
    let rendered = draw(&app);
    assert!(rendered.contains("ship it"), "{rendered}");
    assert!(!rendered.contains("summary (optional"), "{rendered}");
}

#[test]
fn a_reply_composer_names_the_thread_it_answers() {
    let mut app = load();
    app.pane = Pane::Diff;
    let thread = app.pr.as_ref().unwrap().threads[0].clone();
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
    press(&mut app, "j");

    act(&mut app, &Action::StartComment);
    let rendered = draw(&app);
    assert!(rendered.contains("reply ·"), "{rendered}");
}

/// The bar tells trouble from a note by reading the text, so the two shapes
/// that mean trouble have to keep saying so.
#[test]
fn the_status_bar_paints_failures_and_outages_as_trouble() {
    let mut app = App::new();
    assert!(!app.is_status_alarming());

    app.status = "error: fetching changed files failed: HTTP 404".into();
    assert!(app.is_status_alarming());

    app.status = "github major outage".into();
    assert!(app.is_status_alarming());

    // Ordinary notes stay quiet; "no more comments" is not a failure.
    for note in ["draft saved", "review submitted", "no more comments"] {
        app.status = note.into();
        assert!(!app.is_status_alarming(), "{note} should not alarm");
    }
}
