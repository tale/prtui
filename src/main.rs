use anyhow::{Context, Result, bail};
use clap::Parser;
use crossterm::event::{Event, EventStream, KeyEventKind};
use futures_core::Stream;
use prtui::app::App;
use prtui::app::input::InputRouter;
use prtui::model::{self, ChangedFile, PullRequest};
use prtui::{gh, ui};
use std::{future::poll_fn, pin::Pin, time::Instant};
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
}

use prtui::highlight::Segment;

enum Message {
    Meta(Box<PullRequest>),
    Files(Vec<ChangedFile>),
    Highlights(Vec<Vec<Vec<Segment>>>),
    Failed(String),
}

/// Highlighting all files costs ~600ms single-threaded but only ~150ms across
/// cores, which hides entirely inside the network wait.
fn spawn_highlight_pass(files: &[ChangedFile], tx: mpsc::UnboundedSender<Message>) {
    let payload: Vec<(String, Vec<prtui::model::DiffLine>)> = files
        .iter()
        .map(|f| (f.path.clone(), f.lines.clone()))
        .collect();

    std::thread::spawn(move || {
        use rayon::prelude::*;

        let all = payload
            .par_iter()
            .map(|(path, lines)| prtui::highlight::highlight_file(path, lines))
            .collect();

        let _ = tx.send(Message::Highlights(all));
    });
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    let repo = match &args.repo {
        Some(slug) => gh::Repo::parse(slug)?,
        None => gh::current_repo()
            .await
            .context("not inside a GitHub repo; pass -R OWNER/REPO")?,
    };

    // Syntax assets deserialize on a worker so the cost overlaps the fetch.
    std::thread::spawn(prtui::highlight::preload);

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

    run(rx, tx_ui, started).await
}

async fn run(
    mut rx: mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    started: Instant,
) -> Result<()> {
    terminal::scope(async |terminal| event_loop(terminal, &mut rx, tx, started).await).await
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    rx: &mut mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    started: Instant,
) -> Result<()> {
    let mut app = App::new();
    let mut input = InputRouter::default();
    let mut events = EventStream::new();
    let mut pending: u8 = 2;
    let mut failure: Option<String> = None;
    let mut is_dirty = true;

    while !app.should_quit {
        if is_dirty {
            app.ensure_highlighted();
            let pending_hint = input.pending_hint();
            terminal::draw(terminal, |frame| ui::draw(frame, &mut app, &pending_hint))?;
            is_dirty = false;
        }

        tokio::select! {
            message = rx.recv() => {
                let Some(message) = message else {
                    bail!("application message channel closed");
                };

                match message {
                    Message::Highlights(all) => app.set_highlights(all),
                    Message::Meta(pr) => {
                        app.set_meta(*pr);
                        pending = pending.saturating_sub(1);
                    }
                    Message::Files(files) => {
                        spawn_highlight_pass(&files, tx.clone());
                        app.files = files;
                        pending = pending.saturating_sub(1);
                    }
                    Message::Failed(err) => {
                        failure = Some(err);
                        pending = pending.saturating_sub(1);
                    }
                }

                if pending == 0 {
                    app.load_ms = Some(started.elapsed().as_millis());
                    app.status = match &failure {
                        Some(err) => format!("error: {err}"),
                        None => format!(
                            "{} threads",
                            app.threads_by_path.values().flatten().count()
                        ),
                    };
                }

                is_dirty = true;
            }
            event = next_event(&mut events) => {
                let event = event
                    .context("terminal event stream closed")?
                    .context("reading terminal event")?;
                let height = ui::diff_viewport_height(terminal.get_frame().area());

                match event {
                    Event::Resize(_, _) => is_dirty = true,
                    Event::Key(key)
                        if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                    {
                        input.dispatch_key(&mut app, key, height);
                        is_dirty = true;
                    }
                    Event::Paste(text) => {
                        input.dispatch_paste(&mut app, text);
                        is_dirty = true;
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
