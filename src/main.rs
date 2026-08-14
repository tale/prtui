use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use futures_core::Stream;
use prtui::app::App;
use prtui::app::input::InputRouter;
use prtui::model::{self, ChangedFile, PullRequest};
use prtui::renderer::{Renderer, Segment, ThemeMode};
use prtui::{gh, ui};
use std::{future::poll_fn, pin::Pin, time::Duration};
use termina::escape::csi::{Csi, Mode as CsiMode, ThemeMode as TerminalThemeMode};
use termina::event::KeyEventKind;
use termina::{Event, EventStream};
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
            Self::Auto => terminal::detect_theme(),
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
    MetaFailed(String),
    FilesFailed(String),
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

    let repo = match &args.repo {
        Some(slug) => gh::Repo::parse(slug)?,
        None => gh::current_repo()
            .await
            .context("not inside a GitHub repo; pass -R OWNER/REPO")?,
    };

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
                Err(err) => Message::MetaFailed(err.to_string()),
            },
            Err(err) => Message::MetaFailed(err.to_string()),
        };
        let _ = meta_tx.send(msg);
    });

    tokio::spawn(async move {
        let msg = match gh::fetch_files(&repo, number).await {
            Ok(val) => match model::parse_files(&val) {
                Ok(files) => Message::Files(files),
                Err(err) => Message::FilesFailed(err.to_string()),
            },
            Err(err) => Message::FilesFailed(err.to_string()),
        };
        let _ = tx.send(msg);
    });

    // Asking the terminal for its background costs a round trip that can time
    // out; both fetches are already in flight, so it overlaps the network
    // instead of delaying it.
    let renderer = Renderer::new(args.theme.resolve());

    // Syntax assets deserialize on a worker so the cost overlaps the fetch too.
    std::thread::spawn(move || renderer.preload());

    run(rx, tx_ui, renderer, follow_terminal).await
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    renderer: Renderer,
    follow_terminal: bool,
) -> Result<()> {
    terminal::scope(follow_terminal, async |terminal, events| {
        event_loop(terminal, events, &mut rx, tx, renderer, follow_terminal).await
    })
    .await
}

async fn event_loop(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    renderer: Renderer,
    follow_terminal: bool,
) -> Result<()> {
    let mut app = App::with_renderer(renderer);
    let mut input = InputRouter::default();
    let mut pending: u8 = 2;
    let mut failure: Option<String> = None;
    let mut is_dirty = true;
    let mut animation = tokio::time::interval(Duration::from_millis(90));
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !app.should_quit {
        if is_dirty {
            let pending_hint = input.pending_hint();
            terminal::draw(terminal, |frame| ui::draw(frame, &mut app, &pending_hint))?;
            is_dirty = false;
        }

        tokio::select! {
            _ = animation.tick(), if app.is_loading() => {
                app.advance_loading();
                is_dirty = true;
            }
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
                        app.set_files(files);
                        pending = pending.saturating_sub(1);
                        true
                    }
                    Message::MetaFailed(err) => {
                        failure = Some(err);
                        pending = pending.saturating_sub(1);
                        true
                    }
                    Message::FilesFailed(err) => {
                        app.set_files(Vec::new());
                        app.status = format!("error: {err}");
                        failure = Some(err);
                        pending = pending.saturating_sub(1);
                        true
                    }
                };

                if pending_before != 0 && pending == 0 {
                    app.status = match &failure {
                        Some(err) => format!("error: {err}"),
                        None => String::new(),
                    };
                }

                is_dirty |= affects_display;
            }
            event = next_event(events) => {
                let event = event
                    .context("terminal event stream closed")?
                    .context("reading terminal event")?;
                let height = ui::diff_viewport_height(terminal.get_frame().area());

                match event {
                    Event::WindowResized(_) => is_dirty = true,
                    Event::Key(key) => {
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_is_the_only_live_theme_choice() {
        assert!(ThemeChoice::Auto.follows_terminal());
        assert!(!ThemeChoice::Dark.follows_terminal());
        assert!(!ThemeChoice::Light.follows_terminal());
    }
}
