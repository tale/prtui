use prtui::app::action::{Action, Motion};
use prtui::app::draft::Parent;
use prtui::app::input::DispatchResult;
use prtui::app::input::InputRouter;
use prtui::app::review::{Failure, Request, Sent};
use prtui::app::{App, Card, Pane};
use prtui::expand::{Place, Reveal, STEP};
use prtui::layout::Layout;
use prtui::layout::rows::{GUTTER, Row};
use prtui::model::{LineKind, Side, parse_files, parse_meta};
use prtui::renderer::ThemeMode;
use prtui::ui;
use ratatui::Frame;
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::layout::Rect;
use std::fmt::Write;
use std::sync::Arc;
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

/// Renders one frame through the path the event loop uses: layout first, then
/// draw against it.
fn paint(frame: &mut Frame, app: &App) {
    let layout = Layout::compute(frame.area(), app);
    ui::draw(frame, app, &layout, "");
}

/// Drives the app the way the event loop does: raw keys through the keymap.
fn press(app: &mut App, keys: &str) {
    let input = &mut InputRouter::default();
    for c in keys.chars() {
        send(input, app, KeyEvent::new(KeyCode::Char(c), Modifiers::NONE));
    }
}

/// Answers every draft request the way GitHub would, so a test can assert on
/// the state a save or a discard actually settles into.
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

/// Colors the open file the way the background thread does, since that is now
/// the only path into the highlight store.
fn highlight(app: &mut App) {
    let file = &app.files[app.selected_file];
    let styled = prtui::renderer::highlight_file(
        &file.path,
        &file.lines,
        app.theme().mode,
    );
    let path = file.path.clone();

    app.set_highlight(path, styled);
}

/// The fixture's threads in wire order. The app files them by path, so a test
/// that wants one as a template reaches for the fixture rather than the app.
fn fixture_threads() -> Vec<prtui::model::ReviewThread> {
    let meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();

    parse_meta(&meta).unwrap().threads
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

/// The file at head, long enough for any gap in the fixture to open into it.
/// The text is not the real file's, which nothing here depends on: a reveal is
/// addressed by line number.
fn head_of(app: &App) -> Arc<[String]> {
    let file = &app.files[app.selected_file];
    let longest = file
        .lines
        .iter()
        .filter_map(|line| line.new_line)
        .max()
        .unwrap_or(0) as usize;

    (1..=longest + 40)
        .map(|line| format!("head line {line}"))
        .collect()
}

/// Expanding a gap costs one round trip for the file, and the reveal that
/// asked for it is replayed the moment the file lands.
#[test]
fn opening_a_gap_fetches_the_file_and_then_reveals_it() {
    let mut app = load();
    app.pane = Pane::Diff;
    let path = app.files[app.selected_file].path.clone();
    let before = app.files[app.selected_file].lines.len();

    // The fixture's first patch starts at line 127, so everything above it is
    // hidden and the leading gap is the one to open.
    let gaps = app.gaps();
    assert_eq!(gaps[0].place, Place::Leading);
    assert_eq!(gaps[0].len, Some(126));

    act(
        &mut app,
        &Action::Expand {
            gap: 0,
            reveal: Reveal::Up(STEP),
        },
    );

    match app.take_requests().as_slice() {
        [
            Request::Blob {
                path: asked,
                commit,
            },
        ] => {
            assert_eq!(asked, &path);
            assert_eq!(&**commit, "cc36d32a212a2b8b6611fb73549fe6d04fb6ec38");
        }
        other => panic!("expected one blob request, got {other:?}"),
    }

    // Nothing is revealed until the file is in hand.
    assert_eq!(app.files[app.selected_file].lines.len(), before);

    app.finish(Ok(Sent::Blob {
        path: path.clone(),
        lines: head_of(&app),
    }));

    assert_eq!(app.files[app.selected_file].lines.len(), before + 20);
    assert_eq!(app.status, "expanded 20 lines");
    assert!(draw(&app).contains("head line 107"));

    // The patch grew, so the colors held for it no longer describe it.
    assert_eq!(app.take_recolor(), [path]);

    // The file is kept, so the next gap opens without another round trip.
    act(
        &mut app,
        &Action::Expand {
            gap: 1,
            reveal: Reveal::Down(4),
        },
    );
    assert!(app.take_requests().is_empty());
    assert_eq!(app.files[app.selected_file].lines.len(), before + 24);
}

/// A reveal above the cursor moves every line under it, and the cursor has to
/// travel with the line it was resting on rather than stay on a row number.
#[test]
fn a_reveal_carries_the_cursor_with_its_line() {
    let mut app = load();
    app.pane = Pane::Diff;
    app.cursor = 4;
    let path = app.files[app.selected_file].path.clone();
    let resting = app.files[app.selected_file].lines[4].text.clone();

    act(
        &mut app,
        &Action::Expand {
            gap: 0,
            reveal: Reveal::Up(STEP),
        },
    );
    app.take_requests();
    app.finish(Ok(Sent::Blob {
        path,
        lines: head_of(&app),
    }));

    assert_eq!(app.cursor, 24);
    assert_eq!(app.files[app.selected_file].lines[24].text, resting);
}

/// A deleted file is not at head to be read, and its patch already carries
/// every line it ever had.
#[test]
fn a_deleted_file_has_nothing_to_expand_into() {
    let mut app = load();
    let mut files = app.files.to_vec();
    files[0].status = "removed".into();
    app.files = files.into();
    app.selected_file = 0;

    act(
        &mut app,
        &Action::Expand {
            gap: 0,
            reveal: Reveal::Up(STEP),
        },
    );

    assert!(app.take_requests().is_empty());
    assert_eq!(app.status, "no file at head to expand");
}

/// The header stands for the run hidden above it, so it says how much that run
/// holds rather than only where it stops.
#[test]
fn a_hunk_header_says_how_much_it_hides() {
    let mut app = load();
    app.pane = Pane::Diff;
    app.is_files_visible = false;

    assert!(draw(&app).contains("⋯ 126 hidden"));
}

#[test]
fn parses_files_and_threads() {
    let app = load();

    assert_eq!(app.files.len(), 4);
    assert_eq!(app.pr.as_ref().unwrap().number, 9000);
    assert_eq!(app.threads_by_path.values().flatten().count(), 2);

    let threads = fixture_threads();
    let thread = &threads[0];
    assert_eq!(&*thread.id, "PRRT_kwDODKw3uc48Rk4m");
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
    let mut thread = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
    let mut reply = thread.comments[0].clone();
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
    assert!(rendered.contains("1 reply"));
}

#[test]
fn collapsed_thread_summary_uses_rendered_gfm_text() {
    let mut app = load();
    let mut thread = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
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
    let mut thread = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
    let mut reply = thread.comments[0].clone();
    reply.author = "andyfeller".into();
    reply.body = "This is the full reply body.".into();
    thread.comments.push(reply);
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);
    app.focused_card = Some(Card::Thread(thread.id.clone()));
    app.expanded_card = Some(Card::Thread(thread.id.clone()));

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

    assert!(focused_row.contains("◆ ▾ 2 comments"));
    assert!(rendered.contains("Lol not suspicious of coupling at all."));
    assert!(rendered.contains("This is the full reply body."));
    assert!(rendered.contains("↳ @andyfeller"));
    assert!(rendered.contains("collapse"));
}

#[test]
fn expanded_thread_can_scroll_to_its_complete_conversation() {
    let mut app = load();
    let mut thread = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
    thread.comments[0].body =
        paragraphs(18, "Paragraph") + "TAIL CONTENT IS REACHABLE";
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);
    app.focused_card = Some(Card::Thread(thread.id.clone()));
    app.expanded_card = Some(Card::Thread(thread.id.clone()));

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let first_page = terminal.backend().to_string();

    let small_viewport_limit = thread_limit(&app, 100, 24);
    assert!(small_viewport_limit > 0);
    // The rail doubles as the scrollbar, so an overflowing conversation shows
    // a thumb rather than stealing rows for a marker.
    assert!(first_page.contains("┃"));

    app.thread_scroll = small_viewport_limit;
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let last_page = terminal.backend().to_string();

    assert!(last_page.contains("TAIL CONTENT IS REACHABLE"));
    assert_eq!(rail_rows(&first_page), rail_rows(&last_page));

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
    let mut thread = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
    thread.comments[0].body = "Opening comment.".into();
    let mut reply = thread.comments[0].clone();
    reply.author = "andyfeller".into();
    reply.body = paragraphs(24, "Reply paragraph");
    thread.comments.push(reply);
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);
    app.focused_card = Some(Card::Thread(thread.id.clone()));
    app.expanded_card = Some(Card::Thread(thread.id.clone()));

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
    assert!(rendered.contains("· continued"));
}

/// How many rows the open conversation occupies, which must not change as it
/// scrolls: a card that grows or shrinks under the cursor shifts the diff below
/// it on every keystroke.
fn rail_rows(rendered: &str) -> usize {
    rendered
        .lines()
        .filter(|line| line.contains("│") || line.contains("┃"))
        .count()
}

/// A conversation gives the cursor back once it has nothing left to scroll,
/// so one motion walks code, cards and comments without a detour through esc.
#[test]
fn motion_leaves_a_conversation_that_cannot_scroll() {
    let mut app = load();
    let mut thread = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
    thread.comments[0].body = "Short enough to need no scrolling.".into();
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    show_thread(&mut app, &thread);
    app.focused_card = Some(Card::Thread(thread.id.clone()));
    app.expanded_card = Some(Card::Thread(thread.id.clone()));

    let line = app.cursor;
    press(&mut app, "j");

    assert_eq!(app.expanded_card, None);
    assert_eq!(app.focused_card, None);
    assert_eq!(app.cursor, line + 1);
}

#[test]
fn multiple_threads_on_one_line_render_as_one_group() {
    let mut app = load();
    let base = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
    let mut threads = Vec::new();
    for index in 1..=4 {
        let mut thread = base.clone();
        thread.id = format!("thread-{index}").into();
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

    // Every thread is its own card. No heading counts them, no branch glyph
    // makes them look like children of one, and none of them is elided away.
    for index in 1..=4 {
        assert!(
            rendered.contains(&format!(
                "◆ @williammartin  Discussion number {index}"
            )),
            "thread {index} should have its own card"
        );
    }
    assert!(!rendered.contains("open threads"));

    app.focused_card = Some(Card::Thread(threads[3].id.clone()));
    app.expanded_card = Some(Card::Thread(threads[3].id.clone()));
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let expanded = terminal.backend().to_string();

    // Expanding the last one does not push the three above it out of sight,
    // which is what the elision window used to do.
    assert!(expanded.contains("Discussion number 1"));
    assert!(expanded.contains("Discussion number 4"));
    assert!(!expanded.contains("earlier"));
}

#[test]
fn resolved_threads_render_as_compact_rows() {
    let mut app = load();
    let thread = fixture_threads()
        .into_iter()
        .find(|thread| thread.is_resolved)
        .unwrap();
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
    assert!(rendered.contains("resolved"));
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
    let mut thread = fixture_threads().remove(1);
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
    let mut thread = fixture_threads().remove(1);
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
    assert!(rendered.contains("outdated"));
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
    let thread = fixture_threads().remove(1);
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
    use prtui::renderer::ThemeMode;

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

    let styled =
        prtui::renderer::highlight_file("x.rs", &lines, ThemeMode::Dark);

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
    use prtui::renderer::{Theme, ThemeMode};

    let mut app = App::with_theme(Theme::for_mode(ThemeMode::Light));
    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    app.set_files(parse_files(&files).unwrap());
    app.is_files_visible = false;
    highlight(&mut app);

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

    // A long line folds onto several rows, so where a source line landed is the
    // layout's answer rather than an offset into the patch.
    let layout = Layout::compute(Rect::new(0, 0, 100, 30), &app);
    let screen_row = layout.diff.y as usize + layout.rows.code_row(added);

    assert_eq!(
        terminal
            .backend()
            .buffer()
            .cell((0, screen_row as u16))
            .unwrap()
            .style()
            .bg,
        Some(Theme::light().add),
    );

    let colors = app
        .open()
        .unwrap()
        .segments(added)
        .unwrap()
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

/// A late result has to reach the file it was computed for. Addressed by
/// position it would land on whatever happens to sit at that index instead.
#[test]
fn highlights_are_addressed_by_path_not_position() {
    let mut app = load();
    let open = app.current_file().unwrap().path.clone();
    let other = app
        .files
        .iter()
        .map(|file| file.path.clone())
        .find(|path| path != &open)
        .unwrap();

    app.set_highlight(other, vec![Vec::new()]);
    assert!(
        app.open().unwrap().segments(0).is_none(),
        "those are another file's colors"
    );

    app.set_highlight(open, vec![Vec::new()]);
    assert!(app.open().unwrap().segments(0).is_some());

    // The same path can come back carrying a different patch, so a reload
    // cannot keep colors computed against the old one.
    let files = app.files.to_vec();
    app.set_files(files);
    assert!(app.open().unwrap().segments(0).is_none());
}

#[test]
fn switching_terminal_appearance_invalidates_old_syntax_colors() {
    let mut app = load();
    highlight(&mut app);
    assert!(app.open().unwrap().segments(0).is_some());

    assert!(app.set_theme_mode(ThemeMode::Light));
    assert!(app.open().unwrap().segments(0).is_none());
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

    // Commenting on the new file is the ordinary thing to be doing, so the
    // title does not spend room saying so — and never in GitHub's wire word.
    assert!(!rendered.contains("right"), "{rendered}");
    assert!(!rendered.contains("· new"), "{rendered}");
}

/// The old side is the surprising one, so it is the one the title names.
#[test]
fn the_composer_names_the_old_side_and_only_that() {
    let mut app = load();
    app.pane = Pane::Diff;
    app.selected_file = app
        .files
        .iter()
        .position(|file| file.lines.iter().any(|l| l.kind == LineKind::Removed))
        .expect("a file with a deletion");
    app.cursor = app.files[app.selected_file]
        .lines
        .iter()
        .position(|l| l.kind == LineKind::Removed)
        .unwrap();

    act(&mut app, &Action::StartComment);
    let rendered = draw(&app);

    assert!(rendered.contains("· old"), "{rendered}");
    assert!(
        !rendered.contains("left"),
        "never the wire word:\n{rendered}"
    );
}

/// One selected row used to read "1 lines".
#[test]
fn the_selection_counts_one_row_in_the_singular() {
    let mut app = load();
    app.pane = Pane::Diff;
    app.cursor = app.files[app.selected_file]
        .lines
        .iter()
        .position(|l| l.kind != LineKind::Hunk)
        .unwrap();

    act(&mut app, &Action::EnterVisual);
    assert!(draw(&app).contains(" 1 line "), "{}", draw(&app));

    act(&mut app, &Action::Move(Motion::Down(1)));
    assert!(draw(&app).contains(" 2 lines "), "{}", draw(&app));
}

/// The bar used to read `1/4 · 1/8`, which counted two things and named
/// neither.
#[test]
fn the_status_bar_says_what_its_ratios_count() {
    let app = load();
    let rendered = draw(&app);

    assert!(rendered.contains("file 1/4"), "{rendered}");
    assert!(rendered.contains("line 1/8"), "{rendered}");
}

/// The header used to clip mid-word at the pane edge: only the title was
/// budgeted, so the branches and author ran off the end.
#[test]
fn the_header_ends_on_an_ellipsis_rather_than_mid_word() {
    let app = load();

    for width in [50u16, 70, 90, 120] {
        let mut terminal = Terminal::new(TestBackend::new(width, 8)).unwrap();
        terminal.draw(|frame| paint(frame, &app)).unwrap();

        let rendered = terminal.backend().to_string();
        let header = rendered
            .lines()
            .next()
            .unwrap()
            .trim_start_matches('"')
            .trim_end_matches('"');

        assert!(
            header.trim_end().ends_with('…'),
            "at {width} columns: {header:?}"
        );
    }
}

/// Commits `body` as a draft on the first commentable line and reports it.
fn write_draft(app: &mut App, body: &str) -> usize {
    app.pane = Pane::Diff;
    app.cursor = app.files[app.selected_file]
        .lines
        .iter()
        .position(|l| l.kind != LineKind::Hunk)
        .unwrap();

    act(app, &Action::StartComment);
    app.composer.as_mut().unwrap().editor.set_text(body);
    act(app, &Action::CommitComment);
    assert_eq!(app.drafts.len(), 1);

    app.cursor
}

/// The review being written has to be readable in the diff. Before, a draft was
/// a single glyph in the gutter and its body could only be seen by reopening the
/// editor on it, one draft at a time.
#[test]
fn a_saved_draft_shows_its_body_under_the_line() {
    let mut app = load();
    let commented = write_draft(&mut app, "needs a guard");

    let rendered = draw(&app);
    assert!(
        rendered.contains("draft  needs a guard"),
        "the body is on screen:\n{rendered}"
    );
    assert!(
        rendered.contains("· saving"),
        "and a draft GitHub has not answered for says so"
    );

    let layout = layout_of(&app);
    let row = (0..layout.rows.len())
        .find(|&row| matches!(layout.rows.get(row), Some(Row::Draft { .. })))
        .expect("the draft owns a row");

    assert!(
        matches!(
            layout.rows.get(row - 1),
            Some(Row::Code { source, .. }) if *source == commented
        ),
        "it sits directly under the line it answers to"
    );
}

/// The refusal used to be a red glyph and nothing else, so a draft GitHub threw
/// out looked much like one it had accepted.
#[test]
fn a_refused_draft_names_the_reason() {
    let mut app = load();
    write_draft(&mut app, "needs a guard");
    let id = app.drafts[0].id;

    app.finish(Err(Failure::Draft(
        id,
        "line must be part of the diff".into(),
    )));

    let rendered = draw(&app);
    assert!(
        rendered.contains("line must be part of the diff"),
        "the reason is on screen:\n{rendered}"
    );
}

const IMAGE_URL: &str = "https://github.com/user-attachments/assets/shot.png";

fn thread_with_image(app: &mut App) -> prtui::model::ReviewThread {
    let mut thread = fixture_threads()
        .into_iter()
        .find(|thread| !thread.is_resolved)
        .unwrap();
    thread.comments[0].body =
        format!("Before and after:\n\n![shot]({IMAGE_URL})");
    app.threads_by_path
        .insert(thread.path.clone(), vec![thread.clone()]);
    thread
}

/// An attachment reads as the link it was, so a comment built around a
/// screenshot still says what the screenshot was of.
#[test]
fn an_attachment_renders_as_its_alt_text() {
    let mut app = load();
    let thread = thread_with_image(&mut app);
    show_thread(&mut app, &thread);
    app.focused_card = Some(Card::Thread(thread.id.clone()));
    app.expanded_card = Some(Card::Thread(thread.id.clone()));

    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    terminal.draw(|frame| paint(frame, &app)).unwrap();

    assert!(terminal.backend().to_string().contains("▭ shot"));
}

#[test]
fn the_file_tree_marks_files_whose_threads_are_all_resolved() {
    let mut app = load();
    let path = app.files[0].path.clone();
    let mut thread = fixture_threads().remove(1);
    thread.is_resolved = true;
    app.threads_by_path.insert(path, vec![thread; 2]);

    let mut terminal = Terminal::new(TestBackend::new(52, 12)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();
    let rendered = terminal.backend().to_string();

    // The mark sits in a fixed column at the left, right of the cursor bar, so
    // scanning for files that still need reading is one glance down the edge
    // rather than a hunt across names of different lengths.
    // `TestBackend` quotes each row, so the mark is the first column of one.
    let marked: Vec<&str> = rendered
        .lines()
        .map(|line| line.trim_start_matches('"'))
        .filter(|line| line.starts_with('◇'))
        .collect();

    assert_eq!(
        marked.len(),
        1,
        "a settled conversation still marks its file, got:\n{rendered}"
    );
    assert!(marked[0].contains("verify.go"), "{:?}", marked[0]);
}

/// The tree filter and the diff search share a matcher, so the tree can show
/// where the hit landed instead of only which files survived it.
#[test]
fn the_file_filter_paints_its_hit_in_the_path() {
    use prtui::app::input::InputRouter;
    use prtui::renderer::Theme;

    let mut app = load();
    app.is_files_visible = true;
    app.pane = Pane::Files;

    let mut input = InputRouter::default();
    send(
        &mut input,
        &mut app,
        KeyEvent::new(KeyCode::Char('/'), Modifiers::NONE),
    );
    paste(&mut input, &mut app, "check");
    assert_eq!(app.filtered_file_indices().len(), 2);

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal.draw(|frame| paint(frame, &app)).unwrap();

    let theme = Theme::dark();
    let painted: String = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .filter(|cell| cell.style().bg == Some(theme.search))
        .map(ratatui::buffer::Cell::symbol)
        .collect();

    // Both rows have room for their hit now that the tree spends its columns on
    // names. What is elided away is still not painted.
    assert_eq!(painted, "checkcheck");
}

/// The tree used to spend eleven columns on `+31    -0` and elide the path
/// mid-segment, which left `…y.go` on an 80-column terminal.
#[test]
fn the_tree_spends_its_columns_on_names() {
    let app = load();

    for width in [80, 120] {
        let mut terminal = Terminal::new(TestBackend::new(width, 12)).unwrap();
        terminal.draw(|frame| paint(frame, &app)).unwrap();
        let rendered = terminal.backend().to_string();

        for name in ["verify.go", "verify_test.go", "auth_check.go"] {
            assert!(
                rendered.contains(name),
                "{name} is named in full at {width} columns:\n{rendered}"
            );
        }
    }
}

/// The tree budgets two columns for an icon and the space after it. A glyph
/// two columns wide would push everything after it past the pane's edge.
#[test]
fn every_tree_icon_is_one_column() {
    use devicons::{Theme as DeviconTheme, icon_for_file};
    use unicode_width::UnicodeWidthChar;

    let samples = [
        "a.rs",
        "a.go",
        "a.ts",
        "a.tsx",
        "a.py",
        "a.md",
        "a.json",
        "a.yaml",
        "a.toml",
        "a.css",
        "a.html",
        "a.sh",
        "a.c",
        "a.cpp",
        "a.java",
        "a.rb",
        "a.php",
        "a.swift",
        "a.kt",
        "a.lua",
        "a.sql",
        "a.png",
        "a.svg",
        "Dockerfile",
        "Makefile",
        "Cargo.lock",
        "LICENSE",
        "unknown.qqq",
        ".gitignore",
    ];

    for path in samples {
        let icon = icon_for_file(path, &Some(DeviconTheme::Dark));
        assert_eq!(
            UnicodeWidthChar::width(icon.icon),
            Some(1),
            "{path} drew U+{:04X}",
            icon.icon as u32
        );
    }
}

/// A directory every file is under is worth naming once. Paying an indent level
/// and a row to repeat it is what makes a tree cost more room than a flat list.
#[test]
fn the_shared_directory_names_the_pane_rather_than_a_row() {
    let app = load();
    let rendered = draw(&app);

    assert!(
        rendered.contains("Files · 4 · pkg/"),
        "the pane is titled with what every file shares:\n{rendered}"
    );
    assert_eq!(
        layout_of(&app).files.root(),
        Some("pkg/"),
        "and it is not also a row"
    );
}

/// A chain of directories with nothing else in it is one heading, or a deep
/// path spends more columns on indentation than the grouping saves.
#[test]
fn an_unbranching_directory_chain_is_one_heading() {
    let app = load();
    let rendered = draw(&app);

    assert!(
        rendered.contains("cmd/attestation/verify/"),
        "three levels, one row:\n{rendered}"
    );
}

#[test]
fn a_folded_directory_hides_its_files_and_says_how_many() {
    let mut app = load();
    app.pane = Pane::Files;

    // Down onto the first heading, then fold it.
    press(&mut app, "gg");
    assert_eq!(app.tree_directory(), Some("pkg/cmd/attestation/verify/"));

    act(&mut app, &Action::Activate);
    let rendered = draw(&app);

    assert!(
        !rendered.contains("verify_test.go"),
        "the files are folded away:\n{rendered}"
    );
    assert!(
        rendered.contains("▸ 2"),
        "and the heading says how many:\n{rendered}"
    );

    act(&mut app, &Action::Activate);
    assert!(
        draw(&app).contains("verify_test.go"),
        "the same key brings them back"
    );
}

/// Folding a directory must not fold away the reason to open it: the mark its
/// files would have carried rolls up onto the heading standing in for them.
#[test]
fn a_folded_directory_carries_its_files_conversation_mark() {
    let mut app = load();
    app.pane = Pane::Files;

    // The fixture's open thread is on auth_check_test.go, under `cmdutil/`.
    press(&mut app, "G");
    let folded = app.tree_directory().map(str::to_string);
    assert_eq!(folded.as_deref(), None, "G lands on the last file");

    press(&mut app, "k");
    press(&mut app, "k");
    assert_eq!(app.tree_directory(), Some("pkg/cmdutil/"));

    act(&mut app, &Action::Activate);
    let rendered = draw(&app);

    assert!(
        !rendered.contains("auth_check_test"),
        "the files are folded away:\n{rendered}"
    );

    // The diff pane's own title names the same path, so look only at the tree.
    let heading = rendered
        .lines()
        .map(|line| line.trim_start_matches('"'))
        .filter_map(|line| line.split(['│', '┐']).next())
        .find(|column| column.contains("cmdutil/"))
        .expect("the heading is still drawn");

    assert!(
        heading.starts_with('◆'),
        "and it answers for their open threads: {heading:?}"
    );
}

/// Folding is keyed on the directory, not on where the tree happened to draw
/// it, so a fold outlives the refetch that rebuilds the rows.
#[test]
fn a_fold_survives_a_refetch() {
    let mut app = load();
    app.pane = Pane::Files;
    press(&mut app, "gg");
    act(&mut app, &Action::Activate);

    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    app.set_files(parse_files(&files).unwrap());

    assert!(
        !draw(&app).contains("verify_test.go"),
        "the fold is still folded"
    );
}

/// The list used to be measured wide enough to paint over the rule between the
/// panes, so every row that named a file erased it.
#[test]
fn the_tree_keeps_the_rule_between_the_panes() {
    let app = load();
    let mut terminal = Terminal::new(TestBackend::new(120, 12)).unwrap();
    terminal.draw(|frame| paint(frame, &app)).unwrap();

    let rendered = terminal.backend().to_string();
    let rules = rendered.lines().filter(|line| line.contains('│')).count();

    assert!(
        rules >= app.files.len(),
        "every file row still shows the rule:\n{rendered}"
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
    highlight(&mut app);

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

/// The status bar holds one line and a rejection names a field, a rule and the
/// value that broke it, so the overlay is where it has to be readable.
#[test]
fn a_rejected_review_shows_what_github_said_above_the_summary() {
    let mut app = load();
    app.pane = Pane::Diff;

    act(&mut app, &Action::StartSubmit);
    app.submission
        .as_mut()
        .unwrap()
        .editor
        .set_text("please fix");
    act(&mut app, &Action::CommitSubmit);
    app.take_requests();

    let reason = "submitting review failed: HTTP 422: Unprocessable Entity: \
                  pull_request_review_thread.line: must be part of the diff";
    app.finish(Err(Failure::Review(reason.into())));

    let rendered = draw(&app);
    assert!(
        rendered.contains("must be part of the diff"),
        "the end of the reason is the part worth reading:\n{rendered}"
    );
    assert!(
        rendered.contains("please fix"),
        "the summary comes back with it:\n{rendered}"
    );
}

#[test]
fn a_reply_composer_names_the_thread_it_answers() {
    let mut app = load();
    app.pane = Pane::Diff;
    let thread = fixture_threads().remove(0);
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

/// Draws a frame and reports the cells alongside where the caret was parked,
/// since half of wrapping is putting the caret in the right place.
///
/// Read off the buffer rather than `TestBackend`'s string dump, which quotes
/// every row and so shifts each column by one.
fn draw_cells(app: &App) -> (Vec<Vec<char>>, (usize, usize)) {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, app);
        })
        .unwrap();

    let cursor = terminal.get_cursor_position().unwrap();
    let buffer = terminal.backend().buffer();
    let area = buffer.area();
    let cells = (0..area.height)
        .map(|y| {
            (0..area.width)
                .flat_map(|x| buffer[(x, y)].symbol().chars())
                .collect()
        })
        .collect();

    (cells, (cursor.x as usize, cursor.y as usize))
}

/// The first row whose text contains `needle`.
fn row_of(cells: &[Vec<char>], needle: &str) -> usize {
    cells
        .iter()
        .position(|row| row.iter().collect::<String>().contains(needle))
        .unwrap_or_else(|| panic!("{needle} should be on screen"))
}

#[test]
fn a_long_comment_folds_and_carries_the_caret_with_it() {
    let mut app = load();
    // Numbered lines so no diff content can collide with the words typed below.
    app.set_files(vec![numbered_file(20)]);
    app.pane = Pane::Diff;
    press(&mut app, "j");
    act(&mut app, &Action::StartComment);

    paste(
        &mut InputRouter::default(),
        &mut app,
        "the quick brown fox jumps over the lazy dog and then keeps right on \
         running well past the edge of any terminal column",
    );

    let (cells, (caret_x, caret_y)) = draw_cells(&app);

    // Sideways scrolling would have shown only the tail near the caret.
    assert_eq!(
        row_of(&cells, "column"),
        row_of(&cells, "quick") + 1,
        "one typed line folds onto exactly one more row"
    );
    assert_eq!(
        caret_y,
        row_of(&cells, "column"),
        "the caret follows the text onto the folded row"
    );

    let typed: String = cells[caret_y][..caret_x].iter().collect();
    assert!(
        typed.ends_with("column"),
        "the caret sits on the cell after the last character typed, \
         row up to it was {typed:?}"
    );
    assert_eq!(cells[caret_y][caret_x], ' ', "and that cell is still blank");
}

#[test]
fn a_hard_newline_keeps_its_own_row() {
    let mut app = load();
    app.set_files(vec![numbered_file(20)]);
    app.pane = Pane::Diff;
    press(&mut app, "j");
    act(&mut app, &Action::StartComment);

    paste(&mut InputRouter::default(), &mut app, "alpha\n\nomega");

    let (cells, (_, caret_y)) = draw_cells(&app);

    // The blank line between them survives instead of being folded away.
    assert_eq!(row_of(&cells, "omega"), row_of(&cells, "alpha") + 2);
    assert_eq!(caret_y, row_of(&cells, "omega"));
}

#[test]
fn shift_c_writes_a_note_about_the_whole_file() {
    let mut app = load();
    app.pane = Pane::Diff;

    press(&mut app, "C");
    assert_eq!(app.mode, prtui::app::mode::Mode::Insert);
    assert!(draw(&app).contains("file note"));

    paste(
        &mut InputRouter::default(),
        &mut app,
        "this file needs splitting",
    );
    act(&mut app, &Action::CommitComment);

    let draft = app.drafts.first().expect("a draft was saved");
    assert!(draft.is_file_level());
    assert!(draft.rows().is_none(), "a file note owns no rows");
    assert_eq!(draft.body, "this file needs splitting");

    // It has no line to hang under, so it leads the diff pane.
    let rendered = draw(&app);
    assert!(rendered.contains("this file needs splitting"));
}

#[test]
fn a_file_note_is_revised_rather_than_stacked() {
    let mut app = load();
    app.pane = Pane::Diff;

    press(&mut app, "C");
    paste(&mut InputRouter::default(), &mut app, "first thought");
    act(&mut app, &Action::CommitComment);

    press(&mut app, "C");
    assert_eq!(
        app.composer.as_ref().map(|composer| composer.editor.text()),
        Some("first thought".to_string()),
        "reopening loads the existing note"
    );
    paste(&mut InputRouter::default(), &mut app, " and a second");
    act(&mut app, &Action::CommitComment);

    assert_eq!(app.drafts.len(), 1, "one note per file");
    assert_eq!(app.drafts[0].body, "first thought and a second");
}

/// The lines the note answers to keep a mark of their own while it is written.
/// The composer docks below the diff, so without one nothing on screen says
/// what the box in front of you is attached to.
#[test]
fn composing_paints_the_rows_the_comment_will_cover() {
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

    let background = |app: &App| -> Option<ratatui::style::Color> {
        let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
        terminal
            .draw(|frame| {
                paint(frame, app);
            })
            .unwrap();
        let row = Layout::compute(FRAME, app).rows.code_row(4) as u16;

        terminal
            .backend()
            .buffer()
            .cell((45, row + 2))
            .unwrap()
            .style()
            .bg
    };

    let plain = background(&app);
    press(&mut app, "c");
    assert_ne!(
        background(&app),
        plain,
        "the commented row is marked while the composer is open"
    );

    act(&mut app, &Action::CancelComment);
    assert_eq!(background(&app), plain, "and released when it closes");
}

/// A note is a card like any other: the cursor lands on it, enter opens it, and
/// the whole body reads back on the diff instead of one truncated line.
#[test]
fn a_draft_expands_into_its_whole_body() {
    let mut app = load();
    app.pane = Pane::Diff;

    press(&mut app, "C");
    paste(
        &mut InputRouter::default(),
        &mut app,
        "first line\n\nlast line",
    );
    act(&mut app, &Action::CommitComment);

    // Committing leaves the focus on what was just written.
    assert_eq!(app.focused_card, Some(Card::Draft(app.drafts[0].id)));

    let collapsed = draw(&app);
    assert!(collapsed.contains("first line"));
    assert!(
        !collapsed.contains("last line"),
        "a collapsed card shows one line of the note"
    );

    act(&mut app, &Action::Activate);
    let expanded = draw(&app);
    assert!(expanded.contains("first line"));
    assert!(expanded.contains("last line"));
    assert!(expanded.contains("file note"));
}

/// The only way back to a note about the whole file: it hangs under no line, so
/// the cursor cannot reach it and `e` had nothing to find.
#[test]
fn a_focused_file_note_takes_the_edit_and_discard_keys() {
    let mut app = load();
    app.pane = Pane::Diff;

    press(&mut app, "C");
    paste(&mut InputRouter::default(), &mut app, "needs splitting");
    act(&mut app, &Action::CommitComment);
    settle(&mut app);

    press(&mut app, "e");
    assert_eq!(
        app.composer.as_ref().map(|composer| composer.editor.text()),
        Some("needs splitting".to_string())
    );
    act(&mut app, &Action::CancelComment);
    act(&mut app, &Action::CancelComment);

    press(&mut app, "d");
    settle(&mut app);
    assert!(app.drafts.is_empty(), "the note is discarded");
    assert!(app.focused_card.is_none(), "the card it sat on is gone");
}

/// A file-level remark is a thread with `subjectType: FILE`, which is the only
/// shape GitHub accepts for one. The review endpoint has no such field, and
/// sending it there took the whole review down with it.
#[test]
fn a_file_note_ships_without_a_line() {
    let mut app = load();
    app.pane = Pane::Diff;
    press(&mut app, "C");
    paste(&mut InputRouter::default(), &mut app, "whole-file remark");
    act(&mut app, &Action::CommitComment);

    let input = app.drafts[0].to_input(&Parent::Review("PRR_1".into()));

    assert_eq!(input["subjectType"], "FILE");
    assert!(input.get("line").is_none(), "no line to point at");
    assert!(input.get("side").is_none());
    assert_eq!(input["body"], "whole-file remark");
}

/// A patch whose every line names itself, so a test can assert on the exact
/// line it expects to find on screen rather than on a rect.
fn numbered_file(count: usize) -> prtui::model::ChangedFile {
    let mut patch = format!("@@ -1,{count} +1,{count} @@\n");
    for index in 0..count {
        let _ = writeln!(patch, "+line_{index:03}_marker");
    }

    let page = serde_json::json!([[{
        "filename": "numbered.rs",
        "status": "modified",
        "additions": count,
        "deletions": 0,
        "patch": patch.trim_end(),
    }]]);

    parse_files(&page).unwrap().remove(0)
}

#[test]
fn an_open_composer_leaves_the_line_it_anchors_to_on_screen() {
    let mut app = load();
    app.set_files(vec![numbered_file(80)]);
    app.pane = Pane::Diff;
    press(&mut app, "G");

    let anchored = "line_079_marker";
    assert!(draw(&app).contains(anchored));

    // The composer takes rows from the diff rather than floating over them, so
    // the cursor has to be pulled back into what is left of the pane.
    act(&mut app, &Action::StartComment);
    let rendered = draw(&app);

    assert!(
        rendered.contains(anchored),
        "the line being commented on must stay visible"
    );
    assert!(rendered.contains("comment · numbered.rs:80"));
}

#[test]
fn the_submit_form_leaves_the_diff_readable_behind_it() {
    let mut app = load();
    app.set_files(vec![numbered_file(80)]);
    app.pane = Pane::Diff;
    press(&mut app, "G");
    act(&mut app, &Action::StartSubmit);

    let rendered = draw(&app);

    assert!(rendered.contains("submit review"));
    assert!(rendered.contains("summary (optional for approve)"));
    assert!(
        rendered.contains("line_079_marker"),
        "docked, not centred over the diff"
    );

    // A centred box would have left diff on both sides of the border; a docked
    // one spans the pane, so no row mixes the two.
    let border = rendered
        .lines()
        .find(|line| line.contains("submit review"))
        .expect("the form has a titled border");
    assert!(
        !border.contains("line_0"),
        "no diff content shares a row with the form: {border:?}"
    );
}

/// A patch of exactly one added line, so a test can say what that line holds.
fn single_line_file(text: &str) -> prtui::model::ChangedFile {
    let page = serde_json::json!([[{
        "filename": "wide.rs",
        "status": "modified",
        "additions": 1,
        "deletions": 0,
        "patch": format!("@@ -1,1 +1,1 @@\n+{text}"),
    }]]);

    parse_files(&page).unwrap().remove(0)
}

/// Rows the open file's line `source` folds across.
fn fold_count(app: &App, source: usize) -> usize {
    Layout::compute(FRAME, app)
        .rows
        .window(0, usize::MAX)
        .iter()
        .filter(
            |row| matches!(row, Row::Code { source: at, .. } if *at == source),
        )
        .count()
}

#[test]
fn a_diff_line_wider_than_the_pane_folds_instead_of_being_cut_off() {
    let mut app = load();
    let line = format!("HEAD_{}_TAIL", "0123456789".repeat(30));
    app.set_files(vec![single_line_file(&line)]);
    app.is_files_visible = false;
    app.pane = Pane::Diff;

    let rendered = draw(&app);

    assert!(
        rendered.contains("HEAD_"),
        "the line starts where it always did"
    );
    assert!(
        rendered.contains("_TAIL"),
        "and its far end reaches the screen instead of being cut off"
    );
    assert_eq!(
        fold_count(&app, 1),
        3,
        "310 columns over a 107-column budget takes three rows"
    );
}

#[test]
fn only_the_first_row_of_a_folded_line_carries_its_number() {
    let mut app = load();
    app.set_files(vec![single_line_file(&"x".repeat(400))]);
    app.is_files_visible = false;
    app.pane = Pane::Diff;

    let (cells, _) = draw_cells(&app);
    let layout = Layout::compute(FRAME, &app);
    let first = layout.diff.y as usize + layout.rows.code_row(1);

    let numbered: String = cells[first][..GUTTER].iter().collect();
    let continued: String = cells[first + 1][..GUTTER].iter().collect();

    assert!(
        numbered.contains('1'),
        "the line names itself once: {numbered:?}"
    );
    assert!(
        numbered.contains('+'),
        "and carries its sigil: {numbered:?}"
    );

    assert!(
        !continued.contains(|c: char| c.is_ascii_digit()),
        "a continuation must not repeat the line number: {continued:?}"
    );
    assert!(
        !continued.contains('+'),
        "nor the sigil, which would read as a second added line: {continued:?}"
    );
    // The text itself resumes only after the gutter's width.
    assert_eq!(cells[first + 1][GUTTER], 'x');
}

#[test]
fn a_hunk_header_stays_on_one_row() {
    let mut app = load();
    app.set_files(vec![single_line_file(&"y".repeat(400))]);
    app.is_files_visible = false;

    // The header draws a rule across the pane; folding it would break the rule
    // into pieces for no gain.
    assert_eq!(fold_count(&app, 0), 1);
    assert!(fold_count(&app, 1) > 1, "the line beside it does fold");
}

#[test]
fn the_cursor_covers_every_row_of_the_line_it_is_on() {
    let mut app = load();
    app.set_files(vec![single_line_file(&"z".repeat(300))]);
    app.is_files_visible = false;
    app.pane = Pane::Diff;
    press(&mut app, "j");

    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    terminal
        .draw(|frame| {
            paint(frame, &app);
        })
        .unwrap();

    let layout = Layout::compute(FRAME, &app);
    let first = layout.diff.y + layout.rows.code_row(1) as u16;
    let buffer = terminal.backend().buffer();

    // A folded line is one logical row, so the whole of it highlights together
    // rather than only the row carrying the line numbers.
    let cursor_background = buffer.cell((0, first)).unwrap().bg;
    for offset in 1..fold_count(&app, 1) as u16 {
        let cell = buffer.cell((0, first + offset)).unwrap();
        assert_eq!(
            cell.bg, cursor_background,
            "row {offset} of the folded line keeps the highlight"
        );
    }

    // And it is a highlight, not the pane's own background.
    let elsewhere = layout.diff.y + layout.rows.code_row(3) as u16;
    assert_ne!(
        buffer.cell((0, elsewhere)).unwrap().bg,
        cursor_background,
        "a line the cursor is not on is not highlighted"
    );
}

/// The pending review is where drafts live now, so reopening the pull request
/// finds them again rather than starting empty.
#[test]
fn pending_threads_come_back_as_drafts() {
    let files: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/files.json")).unwrap();
    let mut meta: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/meta.json")).unwrap();

    let pr = &mut meta["data"]["repository"]["pullRequest"];
    pr["pendingReview"] = serde_json::json!({ "nodes": [{ "id": "PRR_1" }] });

    let submitted = pr["reviewThreads"]["nodes"][0].clone();
    let mut pending = submitted.clone();
    pending["id"] = "PRRT_pending".into();
    pending["comments"]["nodes"] = serde_json::json!([{
        "id": "PRRC_pending",
        "state": "PENDING",
        "fullDatabaseId": null,
        "author": { "login": "tale" },
        "body": "not sent yet",
        "createdAt": "now",
    }]);
    pr["reviewThreads"]["nodes"] = serde_json::json!([submitted, pending]);

    let mut app = App::new();
    app.set_files(parse_files(&files).unwrap());
    app.set_meta(parse_meta(&meta).unwrap());

    assert_eq!(app.drafts.len(), 1, "the pending thread is a draft");
    assert_eq!(app.drafts[0].body, "not sent yet");
    assert_eq!(app.drafts[0].remote.as_deref(), Some("PRRC_pending"));
    assert!(app.drafts[0].rows().is_some(), "it found its rows again");

    // And it is not also drawn on the diff as a conversation.
    let threads: usize = app.threads_by_path.values().map(Vec::len).sum();
    assert_eq!(threads, 1);
}
