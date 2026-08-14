use anyhow::{bail, Context, Result};
use clap::Parser;
use crossterm::event::{self, Event, KeyEventKind};
use prtui::app::input::InputRouter;
use prtui::app::App;
use prtui::model::{self, ChangedFile, PullRequest};
use prtui::{gh, ui};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

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
    let payload: Vec<(String, Vec<prtui::model::DiffLine>)> =
        files.iter().map(|f| (f.path.clone(), f.lines.clone())).collect();

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
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &mut rx, tx, started).await;
    ratatui::restore();

    result
}

async fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    rx: &mut mpsc::UnboundedReceiver<Message>,
    tx: mpsc::UnboundedSender<Message>,
    started: Instant,
) -> Result<()> {
    let mut app = App::new();
    let mut input = InputRouter::default();
    let mut pending = 2;
    let mut failure: Option<String> = None;
    let mut is_dirty = true;

    while !app.should_quit {
        if is_dirty {
            app.ensure_highlighted();
            let pending_hint = input.pending_hint();
            terminal.draw(|frame| ui::draw(frame, &mut app, &pending_hint))?;
            is_dirty = false;
        }

        while let Ok(msg) = rx.try_recv() {
            is_dirty = true;

            match msg {
                Message::Highlights(all) => {
                    app.set_highlights(all);
                    continue;
                }
                Message::Meta(pr) => app.set_meta(*pr),
                Message::Files(files) => {
                    spawn_highlight_pass(&files, tx.clone());
                    app.files = files;
                }
                Message::Failed(err) => failure = Some(err),
            }

            pending -= 1;
            if pending > 0 {
                continue;
            }

            app.load_ms = Some(started.elapsed().as_millis());
            app.status = match &failure {
                Some(err) => format!("error: {err}"),
                None => format!("{} threads", app.threads_by_path.values().flatten().count()),
            };
        }

        // Idle costs one poll wakeup per tick, not a full repaint.
        if !event::poll(Duration::from_millis(if pending > 0 { 16 } else { 120 }))? {
            continue;
        }

        let height = ui::diff_viewport_height(terminal.get_frame().area());
        match event::read()? {
            Event::Resize(_, _) => is_dirty = true,
            Event::Key(key) if key.kind == KeyEventKind::Press => {
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

    if let Some(err) = failure {
        bail!(err);
    }

    Ok(())
}
