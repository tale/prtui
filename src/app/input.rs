use super::App;
use super::action::Action;
use super::keymap::{Keymap, Resolution};
use super::mode::Mode;
use termina::event::{KeyCode, KeyEvent, Modifiers};

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
        let filter_active = app.file_filter.is_some();
        self.sync_mode(mode);

        let leaves_thread = mode == Mode::Normal
            && app.focused_thread.is_some()
            && matches!(
                (key.code, key.modifiers),
                (KeyCode::Escape, Modifiers::NONE) | (KeyCode::Char('['), Modifiers::CONTROL)
            );
        if leaves_thread {
            let action = Action::LeaveThread;
            app.apply(action.clone(), viewport_height);
            return DispatchResult::Applied(action);
        }

        match self.keymap.resolve(mode, filter_active, key) {
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

                composer.editor.handle_key(key);
                DispatchResult::ForwardedToEditor
            }
            Resolution::Unbound if mode == Mode::Filter => {
                let Some(filter) = app.file_filter.as_mut() else {
                    return DispatchResult::Ignored;
                };

                filter.handle_key(key);
                app.sync_file_filter();
                DispatchResult::ForwardedToEditor
            }
            Resolution::Unbound => DispatchResult::Ignored,
        }
    }

    pub fn dispatch_paste(&mut self, app: &mut App, text: String) -> DispatchResult {
        self.sync_mode(app.mode);

        match app.mode {
            Mode::Insert => {
                let Some(composer) = app.composer.as_mut() else {
                    return DispatchResult::Ignored;
                };
                composer.editor.insert_text(&text);
                DispatchResult::ForwardedToEditor
            }
            Mode::Filter => {
                let Some(filter) = app.file_filter.as_mut() else {
                    return DispatchResult::Ignored;
                };
                filter.insert_text(&text.replace(['\r', '\n'], ""));
                app.sync_file_filter();
                DispatchResult::ForwardedToEditor
            }
            Mode::Normal | Mode::Visual => DispatchResult::Ignored,
        }
    }

    fn sync_mode(&mut self, mode: Mode) {
        if self.mode == Some(mode) {
            return;
        }

        self.keymap.clear();
        self.mode = Some(mode);
    }
}
