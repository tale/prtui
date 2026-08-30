use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use futures_core::Stream;
use prtui::app::App;
use prtui::app::effect::{Effect, Message as AppMessage};
use prtui::app::input::InputRouter;
use prtui::app::link::{Errand, Origin};
use prtui::app::review::{Failure, Request, Sent};
use prtui::layout::Layout;
use prtui::renderer::{self, Theme, ThemeMode};
use prtui::{gh, ui};
use std::{future::poll_fn, pin::Pin, process::Stdio, time::Duration};
use termina::escape::csi::{
    Csi, Mode as CsiMode, ThemeMode as TerminalThemeMode,
};
use termina::event::KeyEventKind;
use termina::{Event, EventStream};
use tokio::sync::mpsc;

mod highlighter;
mod selector;
mod summary;
mod terminal;

#[derive(Parser)]
#[command(
    name = "prtui",
    version,
    about = "Review GitHub pull requests in the terminal"
)]
struct Args {
    /// Pull request number
    number: Option<u32>,

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
    App(AppMessage),
    Highlight(highlighter::Output),
}

/// Pull metadata again so a posted reply or a resolved thread shows up in the
/// diff without a restart.
fn spawn_meta_fetch(
    repo: gh::Repo,
    number: u32,
    generation: u64,
    tx: mpsc::UnboundedSender<Message>,
) {
    tokio::spawn(async move {
        let outcome = gh::fetch_meta(&repo, number)
            .await
            .map(Box::new)
            .map_err(|err| err.to_string());
        let _ = tx.send(Message::App(AppMessage::Meta {
            generation,
            outcome,
        }));
    });
}

fn spawn_files_fetch(
    repo: gh::Repo,
    number: u32,
    tx: mpsc::UnboundedSender<Message>,
) {
    tokio::spawn(async move {
        let outcome = gh::fetch_files(&repo, number)
            .await
            .map_err(|err| err.to_string());
        let _ = tx.send(Message::App(AppMessage::Files(outcome)));
    });
}

/// Asked only after something has already failed, so a healthy session never
/// pays the round trip. Silence means GitHub says it is fine and the failure
/// belongs to this request alone.
fn spawn_outage_probe(repo: gh::Repo, tx: mpsc::UnboundedSender<Message>) {
    tokio::spawn(async move {
        if let Some(summary) = gh::fetch_outage(&repo).await {
            let _ = tx.send(Message::App(AppMessage::Outage(summary)));
        }
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
            Request::AddThread { draft, thread } => {
                gh::add_thread(&repo, thread)
                    .await
                    .map(|added| Sent::ThreadAdded {
                        draft,
                        review: added.review,
                        comment: added.comment,
                    })
                    .map_err(|err| Failure::Draft(draft, err.to_string()))
            }
            Request::UpdateComment {
                draft,
                comment,
                body,
            } => gh::update_comment(&repo, comment, body)
                .await
                .map(|()| Sent::CommentUpdated(draft))
                .map_err(|err| Failure::Draft(draft, err.to_string())),
            Request::DeleteComment { draft, comment } => {
                gh::delete_comment(&repo, comment)
                    .await
                    .map(|()| Sent::CommentDeleted(draft))
                    .map_err(|err| Failure::Draft(draft, err.to_string()))
            }
            Request::Review {
                parent,
                event,
                body,
            } => gh::submit_review(&repo, parent, event, body)
                .await
                .map(|()| Sent::Review)
                .map_err(|err| Failure::Review(err.to_string())),
            Request::Reply { in_reply_to, body } => {
                gh::reply(&repo, number, in_reply_to, body)
                    .await
                    .map(|()| Sent::Reply)
                    .map_err(|err| Failure::Other(err.to_string()))
            }
            Request::Resolve {
                thread_id,
                is_resolved,
            } => gh::set_resolved(&repo, thread_id, is_resolved)
                .await
                .map(|()| Sent::Resolution(is_resolved))
                .map_err(|err| Failure::Other(err.to_string())),
            Request::SetViewed {
                pr,
                path,
                is_viewed,
            } => gh::set_viewed(&repo, pr, path.clone(), is_viewed)
                .await
                .map(|()| Sent::Viewed { path, is_viewed })
                .map_err(|err| Failure::Other(err.to_string())),
            Request::Blob { path, commit } => {
                match gh::fetch_blob(&repo, &path, &commit).await {
                    Ok(text) => Ok(Sent::Blob {
                        path,
                        lines: text.lines().map(str::to_string).collect(),
                    }),
                    Err(err) => Err(Failure::Blob(path, err.to_string())),
                }
            }
        };

        let _ = tx.send(Message::App(AppMessage::Request(outcome)));
    });
}

// Nothing waits on the child; tokio reaps it when the handle drops.
fn open_url(url: &str) -> Result<()> {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };

    tokio::process::Command::new(opener)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to spawn {opener}"))?;

    Ok(())
}

enum Launch {
    Review {
        repo: gh::Repo,
        number: u32,
    },
    /// The scope the listing is read from, which is every pull request the
    /// user is involved in when the process is outside a repository.
    Select(Option<gh::Repo>),
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let follow_terminal = args.theme.follows_terminal();

    let repo = match &args.repo {
        Some(slug) => Some(gh::Repo::parse(slug)?),
        None => gh::current_repo_if_present().await?,
    };

    let launch = match (args.number, repo) {
        (Some(number), repo) => Launch::Review {
            repo: repo
                .context("not inside a GitHub repo; pass -R OWNER/REPO")?,
            number,
        },
        (None, repo) => Launch::Select(repo),
    };

    // Resolve the initial palette before entering the alternate screen. In
    // auto mode the live notification can still update it inside the session.
    let theme = Theme::for_mode(args.theme.resolve());

    run(theme, follow_terminal, launch).await
}

/// What the event loop needs to talk back to GitHub after the first paint.
#[derive(Clone)]
struct Session {
    repo: gh::Repo,
    number: u32,
    /// Where the same pull request is read on the web, which is what a
    /// permalink and the browser are handed.
    origin: Origin,
    follow_terminal: bool,
}

async fn run(
    theme: Theme,
    follow_terminal: bool,
    launch: Launch,
) -> Result<()> {
    terminal::scope(follow_terminal, async move |terminal, events| {
        let mut theme = theme;
        let session = match launch {
            Launch::Review { repo, number } => Session {
                origin: Origin {
                    repo_url: repo.web_url(),
                    number,
                },
                repo,
                number,
                follow_terminal,
            },
            Launch::Select(repo) => {
                let Some(target) = selector::select(
                    terminal,
                    events,
                    repo,
                    &mut theme,
                    follow_terminal,
                )
                .await?
                else {
                    return Ok(());
                };

                Session {
                    origin: Origin {
                        repo_url: target.repo.web_url(),
                        number: target.number,
                    },
                    repo: target.repo,
                    number: target.number,
                    follow_terminal,
                }
            }
        };

        let (tx, mut rx) = mpsc::unbounded_channel();

        // Syntax assets deserialize while the initial requests are in flight.
        std::thread::spawn(move || renderer::preload(theme.mode));

        event_loop(terminal, events, &mut rx, tx, theme, &session).await
    })
    .await
}

async fn event_loop(
    terminal: &mut terminal::AppTerminal,
    events: &mut EventStream,
    rx: &mut mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    theme: Theme,
    session: &Session,
) -> Result<()> {
    let follow_terminal = session.follow_terminal;
    let mut app = App::with_theme(theme);
    app.set_origin(session.origin.clone());
    app.start();
    let mut input = InputRouter::default();
    let highlighter = highlighter::Highlighter::new({
        let tx = tx.clone();
        move |output| {
            let _ = tx.send(Message::Highlight(output));
        }
    });
    let mut is_dirty = true;
    // Replaced by the first frame's own layout before any input is routed
    // against it, since the loop draws before it reads.
    let mut layout = Layout::compute(terminal.get_frame().area(), &app);
    let mut animation = tokio::time::interval(Duration::from_millis(90));
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !app.should_quit {
        for effect in app.take_effects() {
            match effect {
                Effect::FetchFiles => spawn_files_fetch(
                    session.repo.clone(),
                    session.number,
                    tx.clone(),
                ),
                Effect::FetchMeta { generation } => spawn_meta_fetch(
                    session.repo.clone(),
                    session.number,
                    generation,
                    tx.clone(),
                ),
                Effect::ProbeOutage => {
                    spawn_outage_probe(session.repo.clone(), tx.clone());
                }
                Effect::Request(request) => spawn_request(
                    request,
                    session.repo.clone(),
                    session.number,
                    tx.clone(),
                ),
                Effect::HighlightAll => highlighter.all(
                    &app.files,
                    app.selected_file,
                    app.theme().mode,
                ),
                Effect::Highlight(path) => {
                    if let Some(file) =
                        app.files.iter().find(|file| file.path == path)
                    {
                        highlighter.one(file, app.theme().mode);
                    }
                }
                Effect::Errand(Errand::Open(url)) => {
                    if let Err(err) = open_url(&url) {
                        app.status = format!("error: {err}");
                    }
                }
                Effect::Errand(Errand::Copy(text)) => {
                    terminal::copy(terminal, &text)
                        .context("copying to the clipboard")?;
                }
            }
        }

        if is_dirty {
            layout = present_frame(terminal, &app)?;
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

                let affects_display = match message {
                    // A result colored under the previous palette is dropped:
                    // the pass that replaces it is already running.
                    Message::Highlight(output)
                        if output.mode != app.theme().mode
                            || !highlighter.accepts(&output) =>
                    {
                        false
                    }
                    Message::Highlight(output) => {
                        let is_open =
                            app.current_path() == Some(&*output.path);
                        app.set_highlight(output.path, output.styled);
                        is_open
                    }
                    Message::App(message) => app.receive(message),
                };

                is_dirty |= affects_display;
            }
            event = next_event(events) => {
                let event = event
                    .context("terminal event stream closed")?
                    .context("reading terminal event")?;

                match event {
                    Event::WindowResized(_) => is_dirty = true,
                    Event::Key(key) => {
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) {
                            input.dispatch_key(&mut app, key, &layout);
                            is_dirty = true;
                        }
                    }
                    Event::Paste(text) => {
                        input.dispatch_paste(&mut app, &text, &layout);
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
                            is_dirty = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    if let Some(err) = app.take_failure() {
        bail!(err);
    }

    Ok(())
}

/// Draws a frame and hands back the layout it was drawn with, which is what
/// the next keystroke addresses: the cursor and the scroll offset are still
/// pointing at what is on screen.
fn present_frame(
    terminal: &mut terminal::AppTerminal,
    app: &App,
) -> Result<Layout> {
    let layout = Layout::compute(terminal.get_frame().area(), app);

    terminal::render(terminal, |frame| {
        ui::draw(frame, app, &layout);
    })
    .context("drawing a frame")?;

    Ok(layout)
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
    fn pull_request_number_is_optional() {
        assert_eq!(Args::try_parse_from(["prtui"]).unwrap().number, None);
        assert_eq!(
            Args::try_parse_from(["prtui", "42"]).unwrap().number,
            Some(42)
        );
    }

    #[test]
    fn auto_is_the_only_live_theme_choice() {
        assert!(ThemeChoice::Auto.follows_terminal());
        assert!(!ThemeChoice::Dark.follows_terminal());
        assert!(!ThemeChoice::Light.follows_terminal());
    }
}
