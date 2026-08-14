use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::supports_keyboard_enhancement;
use crossterm::{SynchronizedUpdate, execute};
use ratatui::{DefaultTerminal, Frame};
use std::io::stdout;
use std::ops::AsyncFnOnce;

/// Runs asynchronous application code inside a fully restored terminal
/// session. The guard unwinds every mode even when the closure returns early.
pub async fn scope<T, F>(run: F) -> Result<T>
where
    F: for<'a> AsyncFnOnce(&'a mut DefaultTerminal) -> Result<T>,
{
    let mut session = TerminalSession::enter()?;
    run(&mut session.terminal).await
}

/// Draws and presents one complete frame atomically on terminals that support
/// synchronized updates. Older terminals safely ignore the private mode.
pub fn draw(
    terminal: &mut DefaultTerminal,
    render: impl FnOnce(&mut Frame),
) -> std::io::Result<()> {
    stdout().sync_update(|_| terminal.draw(render))??;
    Ok(())
}

struct TerminalSession {
    terminal: DefaultTerminal,
    bracketed_paste: bool,
    enhanced_keyboard: bool,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        let terminal = ratatui::try_init().context("initializing terminal")?;
        let mut session = Self {
            terminal,
            bracketed_paste: false,
            enhanced_keyboard: false,
        };

        execute!(stdout(), EnableBracketedPaste).context("enabling bracketed paste")?;
        session.bracketed_paste = true;

        if supports_keyboard_enhancement().unwrap_or(false) {
            execute!(
                stdout(),
                PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
            )
            .context("enabling enhanced keyboard input")?;
            session.enhanced_keyboard = true;
        }

        Ok(session)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let mut output = stdout();

        if self.enhanced_keyboard {
            let _ = execute!(output, PopKeyboardEnhancementFlags);
        }
        if self.bracketed_paste {
            let _ = execute!(output, DisableBracketedPaste);
        }

        let _ = ratatui::try_restore();
    }
}
