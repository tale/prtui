use anyhow::{Context, Result};
use prtui::renderer::ThemeMode;
use ratatui::backend::TerminaBackend;
use ratatui::{Frame, Terminal as RatatuiTerminal};
use std::cell::RefCell;
use std::io::{self, Write};
use std::ops::AsyncFnOnce;
use std::rc::Rc;
use std::time::Duration;
use termina::escape::csi::{
    Csi, DecPrivateMode, DecPrivateModeCode, Keyboard, KittyKeyboardFlags,
    Mode, ThemeMode as TerminalThemeMode,
};
use termina::escape::osc::{ColorOrQuery, DynamicColorNumber, Osc};
use termina::{
    Event, EventReader, EventStream, PlatformHandle, PlatformTerminal,
    Terminal as TerminaTerminal, WindowSize,
};

pub type AppTerminal = RatatuiTerminal<TerminaBackend<SharedTerminal>>;

/// Lets Ratatui own the output side while the session guard retains access to
/// the same terminal for raw-mode restoration. Only the UI thread touches it.
#[derive(Clone)]
pub struct SharedTerminal(Rc<RefCell<PlatformTerminal>>);

impl SharedTerminal {
    fn open() -> io::Result<Self> {
        Ok(Self(Rc::new(RefCell::new(PlatformTerminal::new()?))))
    }
}

impl Write for SharedTerminal {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.0.borrow_mut().write(buffer)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.borrow_mut().flush()
    }
}

impl TerminaTerminal for SharedTerminal {
    fn enter_raw_mode(&mut self) -> io::Result<()> {
        self.0.borrow_mut().enter_raw_mode()
    }

    fn enter_cooked_mode(&mut self) -> io::Result<()> {
        self.0.borrow_mut().enter_cooked_mode()
    }

    fn get_dimensions(&self) -> io::Result<WindowSize> {
        self.0.borrow().get_dimensions()
    }

    fn event_reader(&self) -> EventReader {
        self.0.borrow().event_reader()
    }

    fn poll<F: Fn(&Event) -> bool>(
        &self,
        filter: F,
        timeout: Option<Duration>,
    ) -> io::Result<bool> {
        self.0.borrow().poll(filter, timeout)
    }

    fn read<F: Fn(&Event) -> bool>(&self, filter: F) -> io::Result<Event> {
        self.0.borrow().read(filter)
    }

    fn set_panic_hook(
        &mut self,
        hook: impl Fn(&mut PlatformHandle) + Send + Sync + 'static,
    ) {
        self.0.borrow_mut().set_panic_hook(hook);
    }
}

/// Query the terminal preference before entering the alternate screen. Modern
/// terminals can answer the semantic query; OSC 11 covers older implementations.
pub fn detect_theme() -> ThemeMode {
    query_theme().ok().flatten().unwrap_or(ThemeMode::Dark)
}

fn query_theme() -> io::Result<Option<ThemeMode>> {
    let mut terminal = PlatformTerminal::new()?;
    terminal.enter_raw_mode()?;

    let result = (|| {
        let query_mode = Csi::Mode(Mode::QueryTheme);
        let query_background = Osc::ChangeDynamicColors(
            DynamicColorNumber::TextBackgroundColor,
            vec![ColorOrQuery::Query],
        );
        write!(terminal, "{query_mode}{query_background}")?;
        terminal.flush()?;

        let filter = |event: &Event| theme_from_event(event).is_some();
        if !terminal.poll(filter, Some(Duration::from_millis(120)))? {
            return Ok(None);
        }
        terminal.read(filter).map(|event| theme_from_event(&event))
    })();

    let restored = terminal.enter_cooked_mode();
    match (result, restored) {
        (Ok(theme), Ok(())) => Ok(theme),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

fn theme_from_event(event: &Event) -> Option<ThemeMode> {
    match event {
        Event::Csi(Csi::Mode(Mode::ReportTheme(TerminalThemeMode::Dark))) => {
            Some(ThemeMode::Dark)
        }
        Event::Csi(Csi::Mode(Mode::ReportTheme(TerminalThemeMode::Light))) => {
            Some(ThemeMode::Light)
        }
        Event::Osc(Osc::ChangeDynamicColors(
            DynamicColorNumber::TextBackgroundColor,
            colors,
        )) => colors.iter().find_map(|color| match color {
            ColorOrQuery::Color(color) => {
                let luma = u32::from(color.red) * 299
                    + u32::from(color.green) * 587
                    + u32::from(color.blue) * 114;
                Some(if luma >= 128_000 {
                    ThemeMode::Light
                } else {
                    ThemeMode::Dark
                })
            }
            ColorOrQuery::Query => None,
        }),
        _ => None,
    }
}

/// Runs asynchronous application code inside a fully restored Termina session.
pub async fn scope<T, F>(follow_theme: bool, run: F) -> Result<T>
where
    F: for<'a> AsyncFnOnce(
        &'a mut AppTerminal,
        &'a mut EventStream,
    ) -> Result<T>,
{
    let mut session = TerminalSession::enter(follow_theme)?;
    run(&mut session.terminal, &mut session.events).await
}

/// Draw one frame inside a synchronized update, so a terminal that supports
/// mode 2026 never shows a half-painted screen. Unsupported terminals ignore it.
pub fn render(
    terminal: &mut AppTerminal,
    render: impl FnOnce(&mut Frame),
) -> io::Result<()> {
    write!(
        terminal.backend_mut(),
        "{}",
        set_mode(DecPrivateModeCode::SynchronizedOutput)
    )?;

    let drawn = terminal.draw(|frame| render(frame)).map(|_| ());

    // The update has to be closed even if the draw failed, or the terminal sits
    // waiting to be told the frame is done.
    write!(
        terminal.backend_mut(),
        "{}",
        reset_mode(DecPrivateModeCode::SynchronizedOutput)
    )?;
    terminal.backend_mut().flush()?;

    drawn
}

struct TerminalSession {
    terminal: AppTerminal,
    events: EventStream,
    control: SharedTerminal,
    follow_theme: bool,
}

impl TerminalSession {
    fn enter(follow_theme: bool) -> Result<Self> {
        let mut control = SharedTerminal::open().context("opening terminal")?;
        control.enter_raw_mode().context("enabling raw mode")?;

        // Termina reopens a pty and restores termios around this; the alternate
        // screen and keyboard flags are ours to undo. The session guard's `Drop`
        // never runs under `panic = "abort"`, so this is the only path back.
        control.set_panic_hook(move |handle| {
            let _ = restore_output(handle, follow_theme);
        });

        let entered = (|| -> Result<(AppTerminal, EventStream)> {
            let mut output = control.clone();

            write!(
                output,
                "{}{}{}",
                set_mode(DecPrivateModeCode::ClearAndEnableAlternateScreen),
                set_mode(DecPrivateModeCode::BracketedPaste),
                Csi::Keyboard(Keyboard::PushFlags(
                    KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
                )),
            )
            .context("configuring terminal")?;

            if follow_theme {
                write!(
                    output,
                    "{}{}",
                    set_mode(DecPrivateModeCode::Theme),
                    Csi::Mode(Mode::QueryTheme),
                )
                .context("enabling terminal theme notifications")?;
            }
            output.flush().context("flushing terminal setup")?;

            let events = EventStream::new(control.event_reader(), |_| true);

            let backend = TerminaBackend::new(output);
            let terminal = RatatuiTerminal::new(backend)
                .context("initializing terminal")?;
            Ok((terminal, events))
        })();

        let (terminal, events) = match entered {
            Ok(session) => session,
            Err(error) => {
                let _ = restore_output(&mut control, follow_theme);
                let _ = control.enter_cooked_mode();
                return Err(error);
            }
        };

        Ok(Self {
            terminal,
            events,
            control,
            follow_theme,
        })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = restore_output(self.terminal.backend_mut(), self.follow_theme);
        let _ = self.control.enter_cooked_mode();
    }
}

fn restore_output(
    output: &mut impl Write,
    follow_theme: bool,
) -> io::Result<()> {
    write!(
        output,
        "{}{}{}",
        Csi::Keyboard(Keyboard::PopFlags(1)),
        reset_mode(DecPrivateModeCode::BracketedPaste),
        reset_mode(DecPrivateModeCode::SynchronizedOutput),
    )?;
    if follow_theme {
        write!(output, "{}", reset_mode(DecPrivateModeCode::Theme))?;
    }
    write!(
        output,
        "{}{}",
        reset_mode(DecPrivateModeCode::ClearAndEnableAlternateScreen),
        set_mode(DecPrivateModeCode::ShowCursor),
    )?;
    output.flush()
}

const fn private_mode(code: DecPrivateModeCode) -> DecPrivateMode {
    DecPrivateMode::Code(code)
}

const fn set_mode(code: DecPrivateModeCode) -> Csi {
    Csi::Mode(Mode::SetDecPrivateMode(private_mode(code)))
}

const fn reset_mode(code: DecPrivateModeCode) -> Csi {
    Csi::Mode(Mode::ResetDecPrivateMode(private_mode(code)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use termina::style::RgbColor;

    #[test]
    fn classifies_osc_11_backgrounds() {
        let report = |color| {
            Event::Osc(Osc::ChangeDynamicColors(
                DynamicColorNumber::TextBackgroundColor,
                vec![ColorOrQuery::Color(color)],
            ))
        };

        assert_eq!(
            theme_from_event(&report(RgbColor::new(13, 17, 23))),
            Some(ThemeMode::Dark)
        );
        assert_eq!(
            theme_from_event(&report(RgbColor::new(255, 255, 255))),
            Some(ThemeMode::Light)
        );
    }

    #[test]
    fn emits_theme_notification_protocol() {
        assert_eq!(
            set_mode(DecPrivateModeCode::Theme).to_string(),
            "\x1b[?2031h"
        );
        assert_eq!(Csi::Mode(Mode::QueryTheme).to_string(), "\x1b[?996n");
        assert_eq!(
            reset_mode(DecPrivateModeCode::Theme).to_string(),
            "\x1b[?2031l"
        );
    }
}
