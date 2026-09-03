use anyhow::{Context, Result, bail};
use prtui_core::{Provider, PullRequestTarget, Repo};
use prtui_tui::renderer::{Theme, ThemeMode};
use prtui_tui::selector::{self, Effect, Message, Selector};
use prtui_tui::terminal;
use std::sync::Arc;
use std::time::Duration;
use termina::escape::csi::{
    Csi, Mode as CsiMode, ThemeMode as TerminalThemeMode,
};
use termina::event::KeyEventKind;
use termina::{Event, EventStream};
use tokio::sync::mpsc;

pub struct Dashboard<P: Provider> {
    selector: Selector,
    provider: P,
    tx: mpsc::UnboundedSender<Message>,
    rx: mpsc::UnboundedReceiver<Message>,
}

impl<P: Provider> Dashboard<P> {
    pub fn new(repo: Option<Repo>, provider: P) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        spawn_listing(repo, provider, tx.clone());

        Self {
            selector: Selector::new(),
            provider,
            tx,
            rx,
        }
    }

    pub async fn select(
        &mut self,
        terminal: &mut terminal::AppTerminal,
        events: &mut EventStream,
        theme: &mut Theme,
        follow_terminal: bool,
    ) -> Result<Option<PullRequestTarget>> {
        self.selector.resume();
        let mut animation = tokio::time::interval(Duration::from_millis(90));
        animation
            .set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        while !self.selector.is_done() {
            terminal::render(terminal, |frame| {
                selector::draw(frame, &self.selector, *theme);
            })
            .context("drawing pull request selector")?;

            let viewport =
                selector::viewport(terminal.get_frame().area(), &self.selector);

            let event = tokio::select! {
                _ = animation.tick(), if self.selector.is_waiting() => {
                    self.selector.advance_loading();
                    continue;
                }
                message = self.rx.recv() => {
                    let Some(message) = message else {
                        bail!("selector message channel closed");
                    };
                    self.selector.receive(message, viewport);
                    continue;
                }
                event = terminal::next_event(events) => {
                    event
                        .context("terminal event stream closed")?
                        .context("reading terminal event")?
                }
            };

            match event {
                Event::Key(key)
                    if matches!(
                        key.kind,
                        KeyEventKind::Press | KeyEventKind::Repeat
                    ) =>
                {
                    let effect = self.selector.press(key, viewport);
                    self.execute(effect);
                }
                Event::Csi(Csi::Mode(CsiMode::ReportTheme(terminal_mode)))
                    if follow_terminal =>
                {
                    let mode = match terminal_mode {
                        TerminalThemeMode::Dark => ThemeMode::Dark,
                        TerminalThemeMode::Light => ThemeMode::Light,
                    };
                    *theme = Theme::for_mode(mode);
                }
                _ => {}
            }
        }

        Ok(self.selector.take_chosen())
    }

    fn execute(&self, effect: Option<Effect>) {
        match effect {
            Some(Effect::Summarize(target)) => {
                spawn_summary(target, self.provider, self.tx.clone());
            }
            Some(Effect::Open(target)) => {
                let url =
                    self.provider.pull_request_url(&target.repo, target.number);
                if let Err(err) = crate::external::open_url(&url) {
                    let _ = self.tx.send(Message::Failed(err.to_string()));
                }
            }
            None => {}
        }
    }
}

fn spawn_listing<P: Provider>(
    repo: Option<Repo>,
    provider: P,
    tx: mpsc::UnboundedSender<Message>,
) {
    tokio::spawn(async move {
        let listed = match repo {
            Some(repo) => provider.repository_pull_requests(repo).await,
            None => provider.user_pull_requests().await,
        }
        .map_err(|err| err.to_string());

        let _ = tx.send(Message::Listed(listed));
    });
}

fn spawn_summary<P: Provider>(
    target: Arc<PullRequestTarget>,
    provider: P,
    tx: mpsc::UnboundedSender<Message>,
) {
    tokio::spawn(async move {
        let summarized = provider
            .fetch_summary(&target.repo, target.number)
            .await
            .map_err(|err| err.to_string());
        let _ = tx.send(Message::Summarized(target, summarized));
    });
}
