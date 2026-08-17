use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use futures_core::Stream;
use prtui::app::App;
use prtui::app::input::InputRouter;
use prtui::app::review::{Request, Sent};
use prtui::images::{self, Image, Images, Support};
use prtui::model::{self, ChangedFile, PullRequest};
use prtui::renderer::{Renderer, Segment, ThemeMode};
use prtui::{gh, ui};
use std::{future::poll_fn, pin::Pin, time::Duration};
use termina::escape::csi::{
    Csi, Mode as CsiMode, ThemeMode as TerminalThemeMode,
};
use termina::event::KeyEventKind;
use termina::{Event, EventStream};
use tokio::sync::mpsc;

mod terminal;

#[derive(Parser)]
#[command(
    name = "prtui",
    about = "Review GitHub pull requests in the terminal"
)]
struct Args {
    /// Pull request number
    number: u32,

    /// Select another repository using the [HOST/]OWNER/REPO format
    #[arg(short = 'R', long = "repo", value_name = "[HOST/]OWNER/REPO")]
    repo: Option<String>,

    /// Color theme; auto queries the terminal's actual background
    #[arg(long, value_enum, default_value_t = ThemeChoice::Auto)]
    theme: ThemeChoice,

    /// Draw comment images with the kitty graphics protocol; auto asks the
    /// terminal whether it supports them
    #[arg(long, value_enum, default_value_t = ImageChoice::Auto)]
    images: ImageChoice,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ImageChoice {
    Auto,
    Always,
    Never,
}

impl ImageChoice {
    const fn queries_terminal(self) -> bool {
        matches!(self, Self::Auto)
    }

    const fn resolve(self, has_graphics: bool) -> Support {
        match self {
            Self::Always => Support::Enabled,
            Self::Never => Support::Disabled,
            Self::Auto if has_graphics => Support::Enabled,
            Self::Auto => Support::Unsupported,
        }
    }
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
    Image(String, Result<Image, String>),
    MetaFailed(String),
    FilesFailed(String),
    /// One outbound request finished, successfully or not.
    Sent(Result<Sent, String>),
}

/// Pull metadata again so a posted reply or a resolved thread shows up in the
/// diff without a restart.
fn spawn_meta_fetch(
    repo: gh::Repo,
    number: u32,
    tx: mpsc::UnboundedSender<Message>,
) {
    tokio::spawn(async move {
        let msg = match gh::fetch_meta(&repo, number).await {
            Ok(val) => match model::parse_meta(&val) {
                Ok(pr) => Message::Meta(Box::new(pr)),
                Err(err) => Message::MetaFailed(err.to_string()),
            },
            Err(err) => Message::MetaFailed(err.to_string()),
        };
        let _ = tx.send(msg);
    });
}

/// Writes go out on their own task so a slow round trip never freezes the
/// review surface behind it.
fn spawn_request(
    request: Request,
    repo: gh::Repo,
    number: u32,
    tx: mpsc::UnboundedSender<Message>,
) {
    tokio::spawn(async move {
        let outcome = match request {
            Request::Review {
                event,
                body,
                comments,
            } => {
                let count = comments.len();
                gh::submit_review(&repo, number, event.as_api(), body, comments)
                    .await
                    .map(|()| Sent::Review(count))
            }
            Request::Reply { in_reply_to, body } => {
                gh::reply(&repo, number, in_reply_to, body)
                    .await
                    .map(|()| Sent::Reply)
            }
            Request::Resolve {
                thread_id,
                is_resolved,
            } => gh::set_resolved(&repo, thread_id, is_resolved)
                .await
                .map(|()| Sent::Resolution(is_resolved)),
        };

        let _ = tx.send(Message::Sent(outcome.map_err(|err| err.to_string())));
    });
}

/// Download and decode off the UI thread; a slow or broken attachment must not
/// stall review.
fn spawn_image_fetch(url: String, tx: mpsc::UnboundedSender<Message>) {
    tokio::spawn(async move {
        let fetched = gh::fetch_asset(&url).await;
        let decoded = match fetched {
            Ok(bytes) => {
                tokio::task::spawn_blocking(move || images::decode(&bytes))
                    .await
                    .unwrap_or_else(|error| Err(error.into()))
            }
            Err(error) => Err(error),
        };

        let _ = tx.send(Message::Image(
            url,
            decoded.map_err(|error| error.to_string()),
        ));
    });
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
    let number = args.number;
    spawn_meta_fetch(repo.clone(), number, tx.clone());

    let files_repo = repo.clone();
    tokio::spawn(async move {
        let msg = match gh::fetch_files(&files_repo, number).await {
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

    let session = Session {
        repo,
        number,
        follow_terminal,
    };

    run(rx, tx_ui, renderer, session, args.images).await
}

/// What the event loop needs to talk back to GitHub after the first paint.
#[derive(Clone)]
struct Session {
    repo: gh::Repo,
    number: u32,
    follow_terminal: bool,
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    renderer: Renderer,
    session: Session,
    images: ImageChoice,
) -> Result<()> {
    let follow_terminal = session.follow_terminal;

    terminal::scope(
        follow_terminal,
        images.queries_terminal(),
        async |terminal, events, has_graphics| {
            event_loop(
                terminal,
                events,
                &mut rx,
                tx,
                renderer,
                &session,
                images.resolve(has_graphics),
            )
            .await
        },
    )
    .await
}

async fn event_loop(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    renderer: Renderer,
    session: &Session,
    support: Support,
) -> Result<()> {
    let follow_terminal = session.follow_terminal;
    let mut app = App::with_renderer(renderer);
    app.images = Images::new(support);
    app.images.set_cell_size(terminal::cell_size(terminal));
    let mut input = InputRouter::default();
    let mut pending: u8 = 2;
    let mut failure: Option<String> = None;
    let mut is_dirty = true;
    let mut animation = tokio::time::interval(Duration::from_millis(90));
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !app.should_quit {
        for url in app.images.take_pending() {
            spawn_image_fetch(url, tx.clone());
        }

        for request in app.take_requests() {
            spawn_request(
                request,
                session.repo.clone(),
                session.number,
                tx.clone(),
            );
        }

        if is_dirty {
            let pending_hint = input.pending_hint();
            terminal::draw(terminal, |frame| {
                ui::draw(frame, &mut app, &pending_hint)
            })?;
            is_dirty = false;
        }

        tokio::select! {
            _ = animation.tick(), if app.is_loading() || app.in_flight > 0 => {
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
                    Message::Image(url, image) => {
                        app.images.insert(url, image);
                        true
                    }
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
                    // Only the first fetch is fatal; a refresh that fails
                    // leaves the review usable with stale threads.
                    Message::MetaFailed(err) if pending > 0 => {
                        failure = Some(err);
                        pending -= 1;
                        true
                    }
                    Message::MetaFailed(err) => {
                        app.status = format!("error: refreshing comments: {err}");
                        true
                    }
                    Message::FilesFailed(err) => {
                        app.set_files(Vec::new());
                        app.status = format!("error: {err}");
                        failure = Some(err);
                        pending = pending.saturating_sub(1);
                        true
                    }
                    // A write only shows up in the diff once the threads are
                    // read back, so a success pulls metadata again.
                    Message::Sent(outcome) => {
                        let is_sent = outcome.is_ok();
                        app.finish(outcome);
                        if is_sent {
                            spawn_meta_fetch(
                                session.repo.clone(),
                                session.number,
                                tx.clone(),
                            );
                        }
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
                    Event::WindowResized(_) => {
                        app.images.set_cell_size(terminal::cell_size(terminal));
                        is_dirty = true;
                    }
                    Event::Key(key) => {
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            input.dispatch_key(&mut app, key, height);
                            is_dirty = true;
                        }
                    }
                    Event::Paste(text) => {
                        input.dispatch_paste(&mut app, &text, height);
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

async fn next_event(
    events: &mut EventStream,
) -> Option<std::io::Result<Event>> {
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
