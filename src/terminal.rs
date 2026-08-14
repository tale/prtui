use anyhow::{Context, Result};
use crossterm::event::{
    DisableBracketedPaste, EnableBracketedPaste, KeyboardEnhancementFlags,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::terminal::supports_keyboard_enhancement;
use crossterm::{SynchronizedUpdate, execute};
use ratatui::{DefaultTerminal, Frame};
use std::io::{Write, stdout};
use std::ops::AsyncFnOnce;

const ENABLE_THEME_NOTIFICATIONS: &[u8] = b"\x1b[?2031h\x1b[?996n";
const DISABLE_THEME_NOTIFICATIONS: &[u8] = b"\x1b[?2031l";

/// Runs asynchronous application code inside a fully restored terminal
/// session. The guard unwinds every mode even when the closure returns early.
pub async fn scope<T, F>(follow_theme: bool, run: F) -> Result<T>
where
    F: for<'a> AsyncFnOnce(&'a mut DefaultTerminal) -> Result<T>,
{
    let mut session = TerminalSession::enter(follow_theme)?;
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
    theme_notifications: bool,
}

impl TerminalSession {
    fn enter(follow_theme: bool) -> Result<Self> {
        let terminal = ratatui::try_init().context("initializing terminal")?;
        let mut session = Self {
            terminal,
            bracketed_paste: false,
            enhanced_keyboard: false,
            theme_notifications: false,
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

        if follow_theme {
            let mut output = stdout();
            output
                .write_all(ENABLE_THEME_NOTIFICATIONS)
                .and_then(|()| output.flush())
                .context("enabling terminal theme notifications")?;
            session.theme_notifications = true;
        }

        Ok(session)
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let mut output = stdout();

        if self.theme_notifications {
            let _ = output.write_all(DISABLE_THEME_NOTIFICATIONS);
            let _ = output.flush();
        }

        if self.enhanced_keyboard {
            let _ = execute!(output, PopKeyboardEnhancementFlags);
        }
        if self.bracketed_paste {
            let _ = execute!(output, DisableBracketedPaste);
        }

        let _ = ratatui::try_restore();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uses_dec_theme_notification_protocol() {
        assert_eq!(ENABLE_THEME_NOTIFICATIONS, b"\x1b[?2031h\x1b[?996n");
        assert_eq!(DISABLE_THEME_NOTIFICATIONS, b"\x1b[?2031l");
    }
}
