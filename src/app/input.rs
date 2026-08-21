use super::App;
use super::action::Action;
use super::keymap::{Keymap, Resolution};
use super::mode::Mode;
use crate::layout::Layout;
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
        layout: &Layout,
    ) -> DispatchResult {
        let mode = app.mode;
        let find_active = app.file_filter.is_some() || app.search.is_some();
        self.sync_mode(mode);

        let leaves_card = mode == Mode::Normal
            && app.focused_card.is_some()
            && matches!(
                (key.code, key.modifiers),
                (KeyCode::Escape, Modifiers::NONE)
                    | (KeyCode::Char('['), Modifiers::CONTROL)
            );
        if leaves_card {
            let action = Action::LeaveThread;
            app.apply(&action, layout);
            return DispatchResult::Applied(action);
        }

        match self.keymap.resolve(mode, find_active, key) {
            Resolution::Action(action) => {
                app.apply(&action, layout);
                self.sync_mode(app.mode);
                DispatchResult::Applied(action)
            }
            Resolution::Pending => DispatchResult::Pending,
            Resolution::Unbound if mode == Mode::Insert => {
                let Some(composer) = app.composer.as_mut() else {
                    return DispatchResult::Ignored;
                };

                composer.is_discard_armed = false;
                composer.editor.handle_key(key);
                DispatchResult::ForwardedToEditor
            }
            Resolution::Unbound if mode == Mode::Submit => {
                let Some(submission) = app.submission.as_mut() else {
                    return DispatchResult::Ignored;
                };

                submission.is_discard_armed = false;
                submission.editor.handle_key(key);
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
            Resolution::Unbound if mode == Mode::Search => {
                let Some(search) = app.search.as_mut() else {
                    return DispatchResult::Ignored;
                };

                search.handle_key(key);
                app.sync_search(layout);
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

        match app.mode {
            Mode::Insert => {
                let Some(composer) = app.composer.as_mut() else {
                    return DispatchResult::Ignored;
                };
                composer.is_discard_armed = false;
                composer.editor.insert_text(text);
                DispatchResult::ForwardedToEditor
            }
            Mode::Submit => {
                let Some(submission) = app.submission.as_mut() else {
                    return DispatchResult::Ignored;
                };
                submission.is_discard_armed = false;
                submission.editor.insert_text(text);
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
            Mode::Search => {
                let Some(search) = app.search.as_mut() else {
                    return DispatchResult::Ignored;
                };
                search.insert_text(&text.replace(['\r', '\n'], ""));
                app.sync_search(layout);
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
