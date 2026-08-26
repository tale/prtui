use super::App;
use super::action::Action;
use super::keymap::Resolution;
use super::mode::Mode;
use crate::layout::Layout;
use termina::event::KeyEvent;

/// What the input router did with an incoming terminal event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResult {
    Applied(Action),
    Pending,
    ForwardedToEditor,
    Ignored,
}

/// Routes keys between the application keymap and whichever line the current
/// mode is editing, and drops a half-typed command when the mode changes under
/// it.
#[derive(Default)]
pub struct InputRouter {
    mode: Option<Mode>,
}

impl InputRouter {
    pub fn dispatch_key(
        &mut self,
        app: &mut App,
        key: KeyEvent,
        layout: &Layout,
    ) -> DispatchResult {
        let mode = app.mode;
        self.sync_mode(app, mode);

        match app.resolve_key(key) {
            Resolution::Action(action) => {
                app.apply(&action, layout);
                let mode = app.mode;
                self.sync_mode(app, mode);
                DispatchResult::Applied(action)
            }
            Resolution::Pending => DispatchResult::Pending,
            Resolution::Unbound if app.type_key(key, layout) => {
                DispatchResult::ForwardedToEditor
            }
            Resolution::Unbound => DispatchResult::Ignored,
        }
    }

    pub fn dispatch_paste(
        &mut self,
        app: &mut App,
        text: &str,
        layout: &Layout,
    ) -> DispatchResult {
        let mode = app.mode;
        self.sync_mode(app, mode);

        if app.type_text(text, layout) {
            return DispatchResult::ForwardedToEditor;
        }

        DispatchResult::Ignored
    }

    fn sync_mode(&mut self, app: &mut App, mode: Mode) {
        if self.mode == Some(mode) {
            return;
        }

        app.clear_pending();
        self.mode = Some(mode);
    }
}
