use super::App;
use super::action::Action;
use super::keymap::{Keymap, Resolution};
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

/// Owns transient input state and routes keys between the application keymap
/// and whichever line the current mode is editing.
#[derive(Default)]
pub struct InputRouter {
    keymap: Keymap,
    mode: Option<Mode>,
}

impl InputRouter {
    pub fn pending_hint(&self) -> String {
        self.keymap.pending_hint()
    }

    pub fn dispatch_key(
        &mut self,
        app: &mut App,
        key: KeyEvent,
        layout: &Layout,
    ) -> DispatchResult {
        let mode = app.mode;
        self.sync_mode(mode);

        match self.keymap.resolve(mode, key) {
            Resolution::Action(action) => {
                app.apply(&action, layout);
                self.sync_mode(app.mode);
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
        self.sync_mode(app.mode);

        if app.type_text(text, layout) {
            return DispatchResult::ForwardedToEditor;
        }

        DispatchResult::Ignored
    }

    fn sync_mode(&mut self, mode: Mode) {
        if self.mode == Some(mode) {
            return;
        }

        self.keymap.clear();
        self.mode = Some(mode);
    }
}
