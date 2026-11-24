use anyhow::{Context, Result};
use futures_core::Stream;
use ratatui::backend::TerminaBackend;
use ratatui::{Frame, Terminal as RatatuiTerminal};
use std::cell::RefCell;
use std::future::poll_fn;
use std::io::{self, Write};
use std::ops::AsyncFnOnce;
use std::pin::Pin;
use std::rc::Rc;
use std::time::{Duration, Instant};
use termina::escape::csi::{
    Csi, DecPrivateMode, DecPrivateModeCode, Device, Keyboard,
    KittyKeyboardFlags, Mode,
};
use termina::escape::osc::{ColorOrQuery, DynamicColorNumber, Osc, Selection};
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

/// Query the terminal background before entering the alternate screen.
pub fn detect_background() -> Option<u32> {
    read_background().ok().flatten()
}

fn read_background() -> io::Result<Option<u32>> {
    let mut terminal = PlatformTerminal::new()?;
    terminal.enter_raw_mode()?;

    let result = (|| {
        // DA1 is very widely supported
        let sentinel = Csi::Device(Device::RequestPrimaryDeviceAttributes);
        write!(terminal, "{}{sentinel}", background_query())?;
        terminal.flush()?;

        let is_sentinel = |event: &Event| {
            matches!(
                event,
                Event::Csi(Csi::Device(Device::DeviceAttributes(())))
            )
        };

        let filter = |event: &Event| {
            background_color(event).is_some() || is_sentinel(event)
        };

        let deadline = Instant::now() + Duration::from_millis(100);
        let mut background = None;

        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !terminal.poll(filter, Some(remaining))? {
                break;
            }

            match background_color(&terminal.read(filter)?) {
                Some(color) => background = Some(color),
                None => break,
            }
        }

        Ok(background)
    })();

    let restored = terminal.enter_cooked_mode();
    match (result, restored) {
        (Ok(background), Ok(())) => Ok(background),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// Asks for the terminal background. The reply arrives as an event for
/// [`background_color`].
pub fn query_background(terminal: &mut AppTerminal) -> io::Result<()> {
    write!(terminal.backend_mut(), "{}", background_query())?;
    terminal.backend_mut().flush()
}

/// Reads the color out of an OSC 11 background report.
pub fn background_color(event: &Event) -> Option<u32> {
    match event {
        Event::Osc(Osc::ChangeDynamicColors(
            DynamicColorNumber::TextBackgroundColor,
            colors,
        )) => colors.iter().find_map(|color| match color {
            ColorOrQuery::Color(color) => Some(
                u32::from(color.red) << 16
                    | u32::from(color.green) << 8
                    | u32::from(color.blue),
            ),
            ColorOrQuery::Query => None,
        }),
        _ => None,
    }
}

fn background_query() -> Osc<'static> {
    Osc::ChangeDynamicColors(
        DynamicColorNumber::TextBackgroundColor,
        vec![ColorOrQuery::Query],
    )
}

/// Runs asynchronous application code inside a fully restored Termina session.
pub async fn scope<T, F>(run: F) -> Result<T>
where
    F: for<'a> AsyncFnOnce(
        &'a mut AppTerminal,
        &'a mut EventStream,
    ) -> Result<T>,
{
    let mut session = TerminalSession::enter()?;
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

/// Puts text on the terminal's own clipboard, which is the only one reachable
/// from the far end of an SSH session.
///
/// A terminal that refuses OSC 52 drops it silently and answers nothing either
/// way, so there is no outcome to report beyond the write itself.
pub fn copy(terminal: &mut AppTerminal, text: &str) -> io::Result<()> {
    write!(
        terminal.backend_mut(),
        "{}",
        Osc::SetSelection(Selection::CLIPBOARD, text)
    )?;
    terminal.backend_mut().flush()
}

/// Waits for the next terminal event.
pub async fn next_event(events: &mut EventStream) -> Option<io::Result<Event>> {
    poll_fn(|cx| Pin::new(&mut *events).poll_next(cx)).await
}

struct TerminalSession {
    terminal: AppTerminal,
    events: EventStream,
    control: SharedTerminal,
}

impl TerminalSession {
    fn enter() -> Result<Self> {
        let mut control = SharedTerminal::open().context("opening terminal")?;
        control.enter_raw_mode().context("enabling raw mode")?;

        // Termina reopens a pty and restores termios around this; the alternate
        // screen and keyboard flags are ours to undo. The session guard's `Drop`
        // never runs under `panic = "abort"`, so this is the only path back.
        control.set_panic_hook(|handle| {
            let _ = restore_output(handle);
        });

        let entered = (|| -> Result<(AppTerminal, EventStream)> {
            let mut output = control.clone();

            write!(
                output,
                "{}{}{}{}",
                set_mode(DecPrivateModeCode::ClearAndEnableAlternateScreen),
                set_mode(DecPrivateModeCode::BracketedPaste),
                Csi::Keyboard(Keyboard::PushFlags(
                    KittyKeyboardFlags::DISAMBIGUATE_ESCAPE_CODES
                )),
                set_mode(DecPrivateModeCode::Theme),
            )
            .context("configuring terminal")?;
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
                let _ = restore_output(&mut control);
                let _ = control.enter_cooked_mode();
                return Err(error);
            }
        };

        Ok(Self {
            terminal,
            events,
            control,
        })
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        let _ = restore_output(self.terminal.backend_mut());
        let _ = self.control.enter_cooked_mode();
    }
}

fn restore_output(output: &mut impl Write) -> io::Result<()> {
    write!(
        output,
        "{}{}{}{}{}{}",
        Csi::Keyboard(Keyboard::PopFlags(1)),
        reset_mode(DecPrivateModeCode::BracketedPaste),
        reset_mode(DecPrivateModeCode::SynchronizedOutput),
        reset_mode(DecPrivateModeCode::Theme),
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
    fn reads_osc_11_backgrounds() {
        let report = |color| {
            Event::Osc(Osc::ChangeDynamicColors(
                DynamicColorNumber::TextBackgroundColor,
                vec![ColorOrQuery::Color(color)],
            ))
        };

        assert_eq!(
            background_color(&report(RgbColor::new(13, 17, 23))),
            Some(0x0d1117)
        );
    }

    #[test]
    fn emits_theme_notification_protocol() {
        assert_eq!(
            set_mode(DecPrivateModeCode::Theme).to_string(),
            "\x1b[?2031h"
        );
        assert_eq!(
            reset_mode(DecPrivateModeCode::Theme).to_string(),
            "\x1b[?2031l"
        );
    }
}
