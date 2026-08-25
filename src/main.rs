use anyhow::{Context, Result, bail};
use clap::{Parser, ValueEnum};
use futures_core::Stream;
use prtui::app::draft::Parent;
use prtui::app::input::InputRouter;
use prtui::app::review::{Failure, Request, Sent};
use prtui::app::{App, Highlight};
use prtui::layout::Layout;
use prtui::model::{self, ChangedFile, Meta};
use prtui::renderer::{self, Theme, ThemeMode};
use prtui::{gh, ui};
use std::sync::Arc;
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
    Meta(Box<Meta>),
    Files(Vec<ChangedFile>),
    Highlight(ThemeMode, Arc<str>, Highlight),
    MetaFailed(String),
    FilesFailed(String),
    /// GitHub is having an incident that explains a failure already shown.
    Outage(String),
    /// One outbound request finished, successfully or not.
    Sent(Result<Sent, Failure>),
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
                Ok(meta) => Message::Meta(Box::new(meta)),
                Err(err) => Message::MetaFailed(err.to_string()),
            },
            Err(err) => Message::MetaFailed(err.to_string()),
        };
        let _ = tx.send(msg);
    });
}

/// Asked only after something has already failed, so a healthy session never
/// pays the round trip. Silence means GitHub says it is fine and the failure
/// belongs to this request alone.
fn spawn_outage_probe(tx: mpsc::UnboundedSender<Message>) {
    tokio::spawn(async move {
        if let Some(summary) = gh::fetch_outage().await {
            let _ = tx.send(Message::Outage(summary));
        }
    });
}

/// A known incident replaces the failure rather than prefixing it: during an
/// outage the HTTP status is noise, and a bare 404 reads like the PR is gone.
fn failure_status(outage: Option<&String>, failure: Option<&String>) -> String {
    if let Some(outage) = outage {
        return outage.clone();
    }

    match failure {
        Some(failure) => format!("error: {failure}"),
        None => String::new(),
    }
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
            Request::AddThread { draft, input, .. } => {
                gh::add_thread(&repo, input)
                    .await
                    .and_then(|val| model::parse_added_thread(&val))
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
            } => {
                let submitted = match parent {
                    Parent::Review(review) => {
                        gh::submit_review(&repo, review, event.as_api(), body)
                            .await
                    }
                    Parent::PullRequest(pr) => {
                        gh::create_review(&repo, pr, event.as_api(), body).await
                    }
                };

                submitted
                    .map(|()| Sent::Review)
                    .map_err(|err| Failure::Review(err.to_string()))
            }
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

        let _ = tx.send(Message::Sent(outcome));
    });
}

/// Colors one file again after its patch grew. Colors are held one list per
/// line, so a reveal strands every line below it.
fn spawn_recolor(
    files: &[ChangedFile],
    path: &Arc<str>,
    mode: ThemeMode,
    tx: mpsc::UnboundedSender<Message>,
) {
    let Some(file) = files.iter().find(|file| file.path == *path) else {
        return;
    };
    let (path, lines) = (file.path.clone(), file.lines.clone());

    std::thread::spawn(move || {
        let styled = renderer::highlight_file(&path, &lines, mode);
        let _ = tx.send(Message::Highlight(mode, path, styled));
    });
}

/// One background thread colors the whole diff, starting with the file on
/// screen so the first paint is already lit. Each file is published as it
/// lands, tagged with the palette it was colored under: a straggler from
/// before a theme switch has to be dropped rather than drawn.
fn spawn_highlighting(
    files: Arc<[ChangedFile]>,
    first: usize,
    mode: ThemeMode,
    tx: mpsc::UnboundedSender<Message>,
) {
    std::thread::spawn(move || {
        for index in highlight_order(files.len(), first) {
            let file = &files[index];
            let styled =
                renderer::highlight_file(&file.path, &file.lines, mode);
            let message = Message::Highlight(mode, file.path.clone(), styled);
            if tx.send(message).is_err() {
                return;
            }
        }
    });
}

/// The file being read first, then everything else in order. Every index it
/// yields is in range, so the worker can index straight into its payload.
fn highlight_order(count: usize, first: usize) -> impl Iterator<Item = usize> {
    std::iter::once(first)
        .filter(move |index| *index < count)
        .chain((0..count).filter(move |index| *index != first))
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
    let theme = Theme::for_mode(args.theme.resolve());

    // Syntax assets deserialize on a worker so the cost overlaps the fetch too.
    std::thread::spawn(move || renderer::preload(theme.mode));

    let session = Session {
        repo,
        number,
        follow_terminal,
    };

    run(rx, tx_ui, theme, session).await
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
    theme: Theme,
    session: Session,
) -> Result<()> {
    let follow_terminal = session.follow_terminal;

    terminal::scope(follow_terminal, async |terminal, events| {
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
    let mut input = InputRouter::default();
    let mut pending: u8 = 2;
    let mut failure: Option<String> = None;
    let mut outage: Option<String> = None;
    let mut is_outage_probed = false;
    let mut is_dirty = true;
    // Replaced by the first frame's own layout before any input is routed
    // against it, since the loop draws before it reads.
    let mut layout = Layout::compute(terminal.get_frame().area(), &app);
    let mut animation = tokio::time::interval(Duration::from_millis(90));
    animation.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    while !app.should_quit {
        for request in app.take_requests() {
            spawn_request(
                request,
                session.repo.clone(),
                session.number,
                tx.clone(),
            );
        }

        for path in app.take_recolor() {
            spawn_recolor(&app.files, &path, app.theme().mode, tx.clone());
        }

        if is_dirty {
            layout = present_frame(terminal, &app, &input.pending_hint())?;
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
                    // A result colored under the previous palette is dropped:
                    // the pass that replaces it is already running.
                    Message::Highlight(mode, ..) if mode != app.theme().mode => {
                        false
                    }
                    Message::Highlight(_, path, styled) => {
                        let is_open = app.current_path() == Some(&*path);
                        app.set_highlight(path, styled);
                        is_open
                    }
                    Message::Meta(meta) => {
                        app.set_meta(*meta);
                        pending = pending.saturating_sub(1);
                        true
                    }
                    Message::Files(files) => {
                        app.set_files(files);
                        spawn_highlighting(
                            app.files.clone(),
                            app.selected_file,
                            app.theme().mode,
                            tx.clone(),
                        );
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
                        app.fail_files();
                        failure = Some(err);
                        app.status =
                            failure_status(outage.as_ref(), failure.as_ref());
                        pending = pending.saturating_sub(1);
                        true
                    }
                    // Lands after the failure it explains, so it rewrites the
                    // line rather than setting one of its own.
                    Message::Outage(summary) => {
                        outage = Some(summary);
                        app.status =
                            failure_status(outage.as_ref(), failure.as_ref());
                        true
                    }
                    // A write only shows up in the diff once the threads are
                    // read back, so a success pulls metadata again.
                    Message::Sent(outcome) => {
                        let is_written =
                            outcome.as_ref().is_ok_and(Sent::is_write);
                        app.finish(outcome);
                        if is_written {
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
                    app.status =
                        failure_status(outage.as_ref(), failure.as_ref());
                }

                if failure.is_some() && !is_outage_probed {
                    is_outage_probed = true;
                    spawn_outage_probe(tx.clone());
                }

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
                            spawn_highlighting(
                                app.files.clone(),
                                app.selected_file,
                                mode,
                                tx.clone(),
                            );
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

/// Draws a frame and hands back the layout it was drawn with, which is what
/// the next keystroke addresses: the cursor and the scroll offset are still
/// pointing at what is on screen.
fn present_frame(
    terminal: &mut terminal::AppTerminal,
    app: &App,
    pending_hint: &str,
) -> Result<Layout> {
    let layout = Layout::compute(terminal.get_frame().area(), app);

    terminal::render(terminal, |frame| {
        ui::draw(frame, app, &layout, pending_hint);
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
    fn the_open_file_is_colored_before_the_rest() {
        assert_eq!(highlight_order(4, 2).collect::<Vec<_>>(), [2, 0, 1, 3]);
        assert_eq!(highlight_order(3, 0).collect::<Vec<_>>(), [0, 1, 2]);
        assert!(highlight_order(0, 0).next().is_none());
    }

    #[test]
    fn auto_is_the_only_live_theme_choice() {
        assert!(ThemeChoice::Auto.follows_terminal());
        assert!(!ThemeChoice::Dark.follows_terminal());
        assert!(!ThemeChoice::Light.follows_terminal());
    }
}
