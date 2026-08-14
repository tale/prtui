use super::App;
use super::action::Action;
use super::keymap::{Keymap, Resolution};
use super::mode::Mode;
use crossterm::event::KeyEvent;
use edtui::EditorEventHandler;

/// What the input router did with an incoming terminal event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchResult {
    Applied(Action),
    Pending,
    ForwardedToEditor,
    Ignored,
}

/// Owns transient input state and routes keys between the application keymap
/// and the embedded comment editor.
#[derive(Default)]
pub struct InputRouter {
    keymap: Keymap,
    editor_events: EditorEventHandler,
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
        viewport_height: usize,
    ) -> DispatchResult {
        let mode = app.mode;
        self.sync_mode(mode);

        match self.keymap.resolve(mode, key) {
            Resolution::Action(action) => {
                app.apply(action.clone(), viewport_height);
                self.sync_mode(app.mode);
                DispatchResult::Applied(action)
            }
            Resolution::Pending => DispatchResult::Pending,
            Resolution::Unbound if mode == Mode::Insert => {
                let Some(composer) = app.composer.as_mut() else {
                    return DispatchResult::Ignored;
                };

                self.editor_events.on_key_event(key, &mut composer.editor);
                DispatchResult::ForwardedToEditor
            }
            Resolution::Unbound => DispatchResult::Ignored,
        }
    }

    pub fn dispatch_paste(&mut self, app: &mut App, text: String) -> DispatchResult {
        self.sync_mode(app.mode);

        if app.mode != Mode::Insert {
            return DispatchResult::Ignored;
        }

        let Some(composer) = app.composer.as_mut() else {
            return DispatchResult::Ignored;
        };

        self.editor_events
            .on_paste_event(text, &mut composer.editor);
        DispatchResult::ForwardedToEditor
    }

    fn sync_mode(&mut self, mode: Mode) {
        if self.mode == Some(mode) {
            return;
        }

        self.keymap.clear();
        self.mode = Some(mode);
    }
}
