use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use futures_core::Stream;
use prtui::app::App;
use prtui::app::input::InputRouter;
use prtui::model::{self, ChangedFile, PullRequest};
use prtui::renderer::{Renderer, Segment, ThemeMode};
use prtui::{gh, ui};
use std::{future::poll_fn, pin::Pin, time::Instant};
use termina::escape::csi::{Csi, Mode as CsiMode, ThemeMode as TerminalThemeMode};
use termina::{Event, EventStream, PlatformTerminal, Terminal};
use tokio::sync::mpsc;

mod terminal;

#[derive(Parser)]
#[command(name = "prtui", about = "Review GitHub pull requests in the terminal")]
struct Args {
    /// Pull request number
    number: u32,

    /// Select another repository using the [HOST/]OWNER/REPO format
    #[arg(short = 'R', long = "repo", value_name = "[HOST/]OWNER/REPO")]
    repo: Option<String>,

    /// Color theme; auto queries the terminal's actual background
    #[arg(long, value_enum, default_value_t = ThemeChoice::Auto)]
    theme: ThemeChoice,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ThemeChoice {
    Auto,
    Dark,
    Light,
}

impl ThemeChoice {
    fn resolve(self) -> ThemeMode {
        match self {
            Self::Auto => ThemeMode::detect(),
            Self::Dark => ThemeMode::Dark,
            Self::Light => ThemeMode::Light,
        }
    }

    const fn follows_terminal(self) -> bool {
        matches!(self, Self::Auto)
    }
}

enum Message {
    Meta(Box<PullRequest>),
    Files(Vec<ChangedFile>),
    Highlight(ThemeMode, usize, Vec<Vec<Segment>>),
    Failed(String),
}

/// Highlighting all files costs ~600ms single-threaded but only ~150ms across
/// cores, which hides entirely inside the network wait.
fn spawn_highlight_pass(
    files: &[ChangedFile],
    renderer: Renderer,
    tx: mpsc::UnboundedSender<Message>,
) {
    let mode = renderer.theme().mode;
    let payload: Vec<(String, Vec<prtui::model::DiffLine>)> = files
        .iter()
        .map(|f| (f.path.clone(), f.lines.clone()))
        .collect();

    std::thread::spawn(move || {
        renderer.highlight_files_parallel(&payload, |index, styled| {
            let _ = tx.send(Message::Highlight(mode, index, styled));
        });
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let follow_terminal = args.theme.follows_terminal();
    let renderer = Renderer::new(args.theme.resolve());

    let repo = match &args.repo {
        Some(slug) => gh::Repo::parse(slug)?,
        None => gh::current_repo()
            .await
            .context("not inside a GitHub repo; pass -R OWNER/REPO")?,
    };

    // Syntax assets deserialize on a worker so the cost overlaps the fetch.
    std::thread::spawn(move || renderer.preload());

    let started = Instant::now();
    let (tx, rx) = mpsc::unbounded_channel();

    // Both round trips leave immediately; whichever lands first paints.
    let tx_ui = tx.clone();
    let meta_tx = tx.clone();
    let meta_repo = repo.clone();
    let number = args.number;
    tokio::spawn(async move {
        let msg = match gh::fetch_meta(&meta_repo, number).await {
            Ok(val) => match model::parse_meta(&val) {
                Ok(pr) => Message::Meta(Box::new(pr)),
                Err(err) => Message::Failed(err.to_string()),
            },
            Err(err) => Message::Failed(err.to_string()),
        };
        let _ = meta_tx.send(msg);
    });

    tokio::spawn(async move {
        let msg = match gh::fetch_files(&repo, number).await {
            Ok(val) => match model::parse_files(&val) {
                Ok(files) => Message::Files(files),
                Err(err) => Message::Failed(err.to_string()),
            },
            Err(err) => Message::Failed(err.to_string()),
        };
        let _ = tx.send(msg);
    });

    run(rx, tx_ui, started, renderer, follow_terminal).await
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    started: Instant,
    renderer: Renderer,
    follow_terminal: bool,
) -> Result<()> {
    terminal::scope(follow_terminal, async |terminal| {
        event_loop(
            terminal,
            &mut rx,
            tx,
            started,
            renderer,
            follow_terminal,
        )
        .await
    })
    .await
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    rx: &mut mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    started: Instant,
    renderer: Renderer,
    follow_terminal: bool,
) -> Result<()> {
    let mut app = App::with_renderer(renderer);
    let mut input = InputRouter::default();
    // Crossterm's renderer remains in place, but Termina is used for input
    // because it exposes private CSI reports such as the live theme event.
    let event_terminal = PlatformTerminal::new().context("opening terminal input")?;
    let mut events = EventStream::new(event_terminal.event_reader(), |_| true);
    let mut pending: u8 = 2;
    let mut failure: Option<String> = None;
    let mut is_dirty = true;

    while !app.should_quit {
        if is_dirty {
            let pending_hint = input.pending_hint();
            terminal::draw(terminal, |frame| ui::draw(frame, &mut app, &pending_hint))?;
            is_dirty = false;
        }

        tokio::select! {
            message = rx.recv() => {
                let Some(message) = message else {
                    bail!("application message channel closed");
                };

                let pending_before = pending;
                let affects_display = match message {
                    Message::Highlight(mode, index, styled) if mode == app.theme().mode => {
                        let is_current = index == app.selected_file;
                        app.set_highlight(index, styled);
                        is_current
                    }
                    Message::Highlight(_, _, _) => false,
                    Message::Meta(pr) => {
                        app.set_meta(*pr);
                        pending = pending.saturating_sub(1);
                        true
                    }
                    Message::Files(files) => {
                        spawn_highlight_pass(
                            &files,
                            Renderer::new(app.theme().mode),
                            tx.clone(),
                        );
                        app.files = files;
                        pending = pending.saturating_sub(1);
                        true
                    }
                    Message::Failed(err) => {
                        failure = Some(err);
                        pending = pending.saturating_sub(1);
                        true
                    }
                };

                if pending_before != 0 && pending == 0 {
                    app.load_ms = Some(started.elapsed().as_millis());
                    app.status = match &failure {
                        Some(err) => format!("error: {err}"),
                        None => format!(
                            "{} threads",
                            app.threads_by_path.values().flatten().count()
                        ),
                    };
                }

                is_dirty |= affects_display;
            }
            event = next_event(&mut events) => {
                let event = event
                    .context("terminal event stream closed")?
                    .context("reading terminal event")?;
                let height = ui::diff_viewport_height(terminal.get_frame().area());

                match event {
                    Event::WindowResized(_) => is_dirty = true,
                    Event::Key(key) => {
                        if let Some(key) = crossterm_key(key)
                            && matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat)
                        {
                            input.dispatch_key(&mut app, key, height);
                            is_dirty = true;
                        }
                    }
                    Event::Paste(text) => {
                        input.dispatch_paste(&mut app, text);
                        is_dirty = true;
                    }
                    Event::Csi(Csi::Mode(CsiMode::ReportTheme(terminal_mode)))
                        if follow_terminal =>
                    {
                        let mode = match terminal_mode {
                            TerminalThemeMode::Dark => ThemeMode::Dark,
                            TerminalThemeMode::Light => ThemeMode::Light,
                        };
                        if app.set_theme_mode(mode) {
                            if !app.files.is_empty() {
                                spawn_highlight_pass(
                                    &app.files,
                                    Renderer::new(mode),
                                    tx.clone(),
                                );
                            }
                            is_dirty = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(err) = failure {
        bail!(err);
    }

    Ok(())
}

async fn next_event(events: &mut EventStream) -> Option<std::io::Result<Event>> {
    poll_fn(|cx| Pin::new(&mut *events).poll_next(cx)).await
}

fn crossterm_key(key: termina::event::KeyEvent) -> Option<KeyEvent> {
    use termina::event::KeyCode as TerminaKeyCode;

    let code = match key.code {
        TerminaKeyCode::Char(c) => KeyCode::Char(c),
        TerminaKeyCode::Enter => KeyCode::Enter,
        TerminaKeyCode::Backspace => KeyCode::Backspace,
        TerminaKeyCode::Tab => KeyCode::Tab,
        TerminaKeyCode::Escape => KeyCode::Esc,
        TerminaKeyCode::Left => KeyCode::Left,
        TerminaKeyCode::Right => KeyCode::Right,
        TerminaKeyCode::Up => KeyCode::Up,
        TerminaKeyCode::Down => KeyCode::Down,
        TerminaKeyCode::Home => KeyCode::Home,
        TerminaKeyCode::End => KeyCode::End,
        TerminaKeyCode::BackTab => KeyCode::BackTab,
        TerminaKeyCode::PageUp => KeyCode::PageUp,
        TerminaKeyCode::PageDown => KeyCode::PageDown,
        TerminaKeyCode::Insert => KeyCode::Insert,
        TerminaKeyCode::Delete => KeyCode::Delete,
        TerminaKeyCode::KeypadBegin => KeyCode::KeypadBegin,
        TerminaKeyCode::CapsLock => KeyCode::CapsLock,
        TerminaKeyCode::ScrollLock => KeyCode::ScrollLock,
        TerminaKeyCode::NumLock => KeyCode::NumLock,
        TerminaKeyCode::PrintScreen => KeyCode::PrintScreen,
        TerminaKeyCode::Pause => KeyCode::Pause,
        TerminaKeyCode::Menu => KeyCode::Menu,
        TerminaKeyCode::Null => KeyCode::Null,
        TerminaKeyCode::Function(n) => KeyCode::F(n),
        // The application has no bindings for standalone modifier or media
        // keys, so there is no reason to expand that conversion surface.
        TerminaKeyCode::Modifier(_) | TerminaKeyCode::Media(_) => return None,
    };

    let mut modifiers = KeyModifiers::NONE;
    let source = key.modifiers;
    for (from, to) in [
        (termina::event::Modifiers::SHIFT, KeyModifiers::SHIFT),
        (termina::event::Modifiers::ALT, KeyModifiers::ALT),
        (termina::event::Modifiers::CONTROL, KeyModifiers::CONTROL),
        (termina::event::Modifiers::SUPER, KeyModifiers::SUPER),
        (termina::event::Modifiers::HYPER, KeyModifiers::HYPER),
        (termina::event::Modifiers::META, KeyModifiers::META),
    ] {
        if source.contains(from) {
            modifiers.insert(to);
        }
    }

    let kind = match key.kind {
        termina::event::KeyEventKind::Press => KeyEventKind::Press,
        termina::event::KeyEventKind::Repeat => KeyEventKind::Repeat,
        termina::event::KeyEventKind::Release => KeyEventKind::Release,
    };

    Some(KeyEvent::new_with_kind(code, modifiers, kind))
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::event::{KeyCode as TerminaKeyCode, KeyEvent as TerminaKeyEvent, Modifiers};

    #[test]
    fn auto_is_the_only_live_theme_choice() {
        assert!(ThemeChoice::Auto.follows_terminal());
        assert!(!ThemeChoice::Dark.follows_terminal());
        assert!(!ThemeChoice::Light.follows_terminal());
    }

    #[test]
    fn termina_keys_keep_bindings_compatible() {
        let source = TerminaKeyEvent::new(
            TerminaKeyCode::Char('c'),
            Modifiers::CONTROL | Modifiers::SHIFT,
        );
        let converted = crossterm_key(source).expect("ordinary key converts");
        assert_eq!(converted.code, KeyCode::Char('c'));
        assert!(converted.modifiers.contains(KeyModifiers::CONTROL));
        assert!(converted.modifiers.contains(KeyModifiers::SHIFT));
    }
}
