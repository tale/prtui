use super::draft::{self, Attachment, Draft, Sync};
use super::editor::CommentEditor;
use super::effect::Effect;
use super::{App, Card, Composer, Mode, Pane, Selection, Target};
use crate::layout::Layout;
use crate::model::{NewThread, Parent, ReviewThread};
use std::sync::Arc;

pub use crate::model::ReviewEvent;

/// The submit overlay: the verdict plus the summary that accompanies it.
///
/// `error` is what GitHub said the last time this review went out. It lives
/// here rather than in the status bar because the bar has one line and a
/// validation failure names a field, a rule and an offending value.
#[derive(Default)]
pub struct Submission {
    pub editor: CommentEditor,
    pub event: ReviewEvent,
    pub error: Option<String>,
    /// Set by an escape that had a summary to lose. The next escape discards;
    /// any other key clears it.
    pub is_discard_armed: bool,
}

/// Work that has to leave the process. The app queues these rather than
/// reaching for the network itself, which keeps every state transition
/// synchronous and testable.
///
/// Every draft request names the draft by its local id, since the answer has to
/// find its way back to a draft that was already on screen before it left.
#[derive(Debug, PartialEq, Eq)]
pub enum Request {
    AddThread {
        draft: u64,
        thread: NewThread,
    },
    UpdateComment {
        draft: u64,
        comment: Arc<str>,
        body: String,
    },
    DeleteComment {
        draft: u64,
        comment: Arc<str>,
    },
    Review {
        parent: Parent,
        event: ReviewEvent,
        body: String,
    },
    Reply {
        in_reply_to: u64,
        body: String,
    },
    Resolve {
        thread_id: Arc<str>,
        is_resolved: bool,
    },
    /// The read-through mark on one file. GitHub hangs it off the pull request
    /// node, so the request carries that rather than the changed file.
    SetViewed {
        pr: Arc<str>,
        path: Arc<str>,
        is_viewed: bool,
    },
    /// One file as it stands at head, which is what a diff expands into the
    /// runs of it the patch left out.
    Blob {
        path: Arc<str>,
        commit: Arc<str>,
    },
}

/// What a completed request retires, so the app knows which local state the
/// server has now taken over.
#[derive(Debug, PartialEq, Eq)]
pub enum Sent {
    ThreadAdded {
        draft: u64,
        review: Arc<str>,
        comment: Arc<str>,
    },
    CommentUpdated(u64),
    CommentDeleted(u64),
    Review,
    Reply,
    Resolution(bool),
    /// The path is what the mark is held against, since the app keeps this one
    /// itself rather than reading it back.
    Viewed {
        path: Arc<str>,
        is_viewed: bool,
    },
    Blob {
        path: Arc<str>,
        lines: Arc<[String]>,
    },
}

impl Sent {
    /// Whether the change only shows once GitHub is read back. Reading a file
    /// changes nothing there, and a viewed mark is the one piece of server
    /// state the app carries itself, so neither is worth a metadata fetch.
    pub const fn needs_refetch(&self) -> bool {
        !matches!(self, Self::Blob { .. } | Self::Viewed { .. })
    }

    /// Whether a metadata fetch already in flight is now out of date. It left
    /// before this write and answers with the state the write replaced, so a
    /// mark the app holds would be taken straight back off the file.
    pub const fn invalidates_fetch(&self) -> bool {
        !matches!(self, Self::Blob { .. })
    }
}

/// Why a request came back empty-handed.
///
/// A review leaves its summary behind, and a draft is left marked as ahead of
/// the server; both have to be handed back rather than dropped, so both are
/// told apart from the failures that only need reporting.
#[derive(Debug, PartialEq, Eq)]
pub enum Failure {
    Draft(u64, String),
    Review(String),
    /// A file whose contents never arrived, named so the expansion waiting on
    /// them stops waiting.
    Blob(Arc<str>, String),
    Other(String),
}

impl Failure {
    pub fn message(&self) -> &str {
        let (Self::Draft(_, message)
        | Self::Review(message)
        | Self::Blob(_, message)
        | Self::Other(message)) = self;

        message
    }
}

impl App {
    /// A focused thread takes a reply; anything else starts a fresh draft over
    /// the cursor line or the visual selection. A focused draft of the reader's
    /// own is not one of those: `c` composes and `e` revises, so a drafted line
    /// still takes a second comment.
    pub(super) fn start_comment(&mut self, layout: &Layout) {
        if self.pane != Pane::Diff {
            return;
        }

        if let Some(id) = self.focused_card.as_ref().and_then(Card::thread) {
            let id = id.clone();
            self.start_reply(&id, layout);
            return;
        }

        let rows = match self.selection {
            Some(selection) => selection.range(),
            None => self.cursor..=self.cursor,
        };

        let Some(file) = self.current_file() else {
            return;
        };
        let path = file.path.clone();
        let Some(anchor) = draft::anchor_for(file, rows.clone()) else {
            self.status = "cannot comment on that line".into();
            return;
        };

        // The rows the note will cover stay painted while it is written, so the
        // composer never floats free of what it answers to.
        self.selection = Some(Selection {
            anchor: *rows.start(),
            head: *rows.end(),
        });
        self.composer = Some(Composer::new(
            CommentEditor::default(),
            Target::Line {
                anchor,
                rows,
                replacing: None,
            },
            path,
        ));
        self.mode = Mode::Insert;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    /// A file takes a single remark, so `C` revises the existing one rather than
    /// stacking another. Available from the tree too: no line is involved, so
    /// there is nothing the diff pane is needed for. The focus follows it over
    /// to the diff, since that is where the typing is about to happen.
    pub(super) fn start_file_comment(&mut self, layout: &Layout) {
        let Some(path) = self.current_file().map(|file| file.path.clone())
        else {
            self.status = "no file selected".into();
            return;
        };

        let existing = self
            .drafts
            .iter()
            .find(|draft| draft.path == path && draft.is_file_level());

        if let Some(draft) = existing {
            let id = draft.id;
            self.reopen_draft(id, layout);
            return;
        }

        self.pane = Pane::Diff;
        self.composer = Some(Composer::new(
            CommentEditor::default(),
            Target::File { replacing: None },
            path,
        ));
        self.mode = Mode::Insert;
        self.selection = None;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    /// Reopens the focused draft, or the one under the cursor, with its body
    /// and its span intact, so committing revises it instead of stacking a
    /// second comment.
    pub(super) fn edit_draft(&mut self, layout: &Layout) {
        let Some(index) = self.editable_draft() else {
            self.status = "no draft here".into();
            return;
        };

        let id = self.drafts[index].id;
        self.reopen_draft(id, layout);
    }

    /// Puts an existing draft back in the composer, whatever it is attached to.
    fn reopen_draft(&mut self, id: u64, layout: &Layout) {
        let Some(index) = self.draft_by_id(id) else {
            return;
        };

        let draft = &self.drafts[index];
        let target = match draft.attachment.clone() {
            Attachment::Lines { rows, anchor } => {
                self.selection = Some(Selection {
                    anchor: *rows.start(),
                    head: *rows.end(),
                });
                Target::Line {
                    anchor,
                    rows,
                    replacing: Some(id),
                }
            }
            Attachment::File => {
                self.selection = None;
                Target::File {
                    replacing: Some(id),
                }
            }
        };

        let mut editor = CommentEditor::default();
        editor.set_text(&draft.body);

        self.pane = Pane::Diff;
        self.composer = Some(Composer::new(editor, target, draft.path.clone()));
        self.mode = Mode::Insert;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    fn start_reply(&mut self, id: &str, layout: &Layout) {
        let Some(thread) = self.thread(id) else {
            return;
        };
        let Some(in_reply_to) = thread.reply_target() else {
            self.status = "this thread cannot be replied to".into();
            return;
        };

        self.composer = Some(Composer::new(
            CommentEditor::default(),
            Target::Reply { in_reply_to },
            thread.path.clone(),
        ));
        self.mode = Mode::Insert;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    pub(super) fn thread(&self, id: &str) -> Option<&ReviewThread> {
        self.threads_by_path
            .values()
            .flatten()
            .find(|thread| *thread.id == *id)
    }

    /// The open file's threads, which is what the row list indexes into.
    pub fn file_threads(&self) -> &[ReviewThread] {
        self.current_file()
            .and_then(|file| self.threads_by_path.get(&file.path))
            .map_or(&[], Vec::as_slice)
    }

    fn draft_at_cursor(&self) -> Option<usize> {
        let path = &self.current_file()?.path;

        self.drafts
            .iter()
            .position(|draft| draft.covers(path, self.cursor))
    }

    /// The draft `e` and `d` act on: the focused one when a card holds the
    /// focus, which is the only way to reach a file note, and otherwise the one
    /// covering the cursor line.
    fn editable_draft(&self) -> Option<usize> {
        if self.focused_card.is_some() {
            return self.focused_draft();
        }
        if self.pane != Pane::Diff {
            return None;
        }

        self.draft_at_cursor()
    }

    pub(super) fn delete_draft(&mut self) {
        let Some(index) = self.editable_draft() else {
            self.status = "no draft here".into();
            return;
        };

        self.discard_draft(index);
        self.prune_focus();
    }

    /// Discards a draft, which means asking GitHub to drop the comment holding
    /// it. One whose own creation is still out has no comment id to name yet,
    /// so it is marked and the discard rides on that answer.
    fn discard_draft(&mut self, index: usize) {
        let draft = &mut self.drafts[index];

        if let Sync::Creating { .. } = draft.sync {
            draft.sync = Sync::Deleting;
            self.status = "discarding draft…".into();
            return;
        }

        let Some(comment) = draft.remote.clone() else {
            self.drafts.remove(index);
            self.status = "draft discarded".into();
            return;
        };

        let id = draft.id;
        draft.sync = Sync::Deleting;
        self.retired.insert(comment.clone());
        self.send(Request::DeleteComment { draft: id, comment });
        self.status = "discarding draft…".into();
    }

    pub(super) fn toggle_resolved(&mut self) {
        let Some(id) =
            self.focused_card.as_ref().and_then(Card::thread).cloned()
        else {
            self.status = "no thread selected".into();
            return;
        };
        let Some(thread) = self.thread(&id) else {
            return;
        };

        if !thread.can_resolve {
            self.status = "you cannot resolve this thread".into();
            return;
        }

        let is_resolved = !thread.is_resolved;
        self.send(Request::Resolve {
            thread_id: id,
            is_resolved,
        });
        self.status = if is_resolved {
            "resolving…".into()
        } else {
            "unresolving…".into()
        };
    }

    /// Marking a file read moves on to the next one, since being done with a
    /// file and wanting to keep looking at it are not the same intent. Clearing
    /// the mark stays put: it is asked for by a reader coming back to the file.
    pub(super) fn toggle_viewed(&mut self, layout: &Layout) {
        let Some(path) = self.current_file().map(|file| file.path.clone())
        else {
            self.status = "no file open".into();
            return;
        };
        let Some(pr) = self.pr.as_ref().map(|pr| pr.id.clone()) else {
            return;
        };

        let is_viewed = !self.viewed.contains(&path);
        self.send(Request::SetViewed {
            pr,
            path,
            is_viewed,
        });

        if !is_viewed {
            self.status = "marking unviewed…".into();
            return;
        }

        self.status = match self.unread_after_current(layout) {
            Some(index) => {
                self.select_file(index);
                "marking viewed…".into()
            }
            None => "marking viewed… nothing left unread".into(),
        };
    }

    /// Takes the mark over from GitHub, which has just confirmed it.
    ///
    /// This is the one piece of server state the app keeps rather than reads
    /// back: a mark says nothing about the threads, so refetching the whole
    /// review to learn one boolean would cost a round trip per file read.
    fn mark_viewed(&mut self, path: Arc<str>, is_viewed: bool) -> String {
        if is_viewed {
            self.viewed.insert(path);
            return "file marked viewed".into();
        }

        self.viewed.remove(&path);
        "file marked unviewed".into()
    }

    pub(super) fn start_submit(&mut self, layout: &Layout) {
        self.composer = None;
        self.selection = None;
        // A review GitHub rejected comes back with the summary that was typed
        // for it, so a second attempt revises rather than retypes.
        let rejected = self.sending.take_if(|held| held.error.is_some());
        self.submission = Some(rejected.unwrap_or_default());
        self.mode = Mode::Submit;
        self.scroll_into_view(layout, layout.viewport_once_docked());
    }

    /// One review goes out at a time. A second would post twice, since the
    /// drafts only retire once the first is answered.
    fn is_review_sending(&self) -> bool {
        self.sending
            .as_ref()
            .is_some_and(|held| held.error.is_none())
    }

    /// A summary is no cheaper to retype than a comment, so escape warns once
    /// here too rather than throwing it away on the first key.
    pub(super) fn cancel_submit(&mut self) {
        let Some(submission) = self.submission.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };

        let has_summary = !submission.editor.text().trim().is_empty();
        if has_summary && !submission.is_discard_armed {
            submission.is_discard_armed = true;
            self.status = "esc again to discard".into();
            return;
        }

        self.submission = None;
        self.mode = Mode::Normal;
        self.status.clear();
    }

    /// A refused submission leaves the overlay open, so a missing summary is
    /// typed rather than retyped.
    pub(super) fn commit_submit(&mut self) {
        let Some(mut submission) = self.submission.take() else {
            return;
        };

        let event = submission.event;
        let body = submission.editor.trimmed_text();

        // Drafts are already on GitHub, so the review that publishes them is
        // the one the app opened for them. Without any, the verdict rides alone
        // and files a review of its own.
        let parent = match (&self.pending_review, self.pr.as_ref()) {
            (Some(review), _) => Some(Parent::Review(review.clone())),
            (None, Some(pr)) => Some(Parent::PullRequest(pr.id.clone())),
            (None, None) => None,
        };

        // An approval is a verdict in itself, so a bare one with no summary and
        // no inline comments is the whole point rather than an empty review.
        let refusal = if self.is_review_sending() {
            Some("a review is already going out".to_string())
        } else if body.is_empty() && event.requires_body() {
            Some(format!("{} needs a summary", event.label()))
        } else if self.is_draft_in_flight() {
            Some("a draft is still saving".to_string())
        } else if parent.is_none() {
            Some("the pull request has not loaded yet".to_string())
        } else {
            None
        };

        let (Some(parent), None) = (parent, refusal.as_ref()) else {
            self.status = refusal.unwrap_or_default();
            self.submission = Some(submission);
            return;
        };

        submission.error = None;
        self.sending = Some(submission);
        self.mode = Mode::Normal;

        self.send(Request::Review {
            parent,
            event,
            body,
        });
        self.status = format!("submitting {}…", event.label());
    }

    pub(super) fn send(&mut self, request: Request) {
        self.effects.push(Effect::Request(request));
        self.in_flight += 1;
    }

    /// Compatibility view for model-level tests that inspect only requests.
    /// Runtime code drains [`Self::take_effects`] instead.
    pub fn take_requests(&mut self) -> Vec<Request> {
        self.take_selected(|effect| match effect {
            Effect::Request(request) => Ok(request),
            effect => Err(effect),
        })
    }

    /// Reports one request's outcome. Drafts survive a failed submission so the
    /// review can be sent again rather than retyped.
    pub fn finish(&mut self, outcome: Result<Sent, Failure>) {
        self.in_flight = self.in_flight.saturating_sub(1);

        self.status = match outcome {
            Ok(Sent::ThreadAdded {
                draft,
                review,
                comment,
            }) => self.draft_created(draft, review, comment),
            Ok(Sent::CommentUpdated(draft)) => self.draft_settled(draft),
            Ok(Sent::CommentDeleted(draft)) => {
                if let Some(index) = self.draft_by_id(draft) {
                    self.drafts.remove(index);
                }

                "draft discarded".into()
            }
            Ok(Sent::Review) => {
                // Everything it carried is GitHub's now, and the refetch that
                // follows brings the whole review back as submitted threads.
                self.drafts.clear();
                self.pending_review = None;
                self.sending = None;
                "review submitted".into()
            }
            Ok(Sent::Reply) => "reply posted".into(),
            Ok(Sent::Resolution(true)) => "thread resolved".into(),
            Ok(Sent::Resolution(false)) => "thread unresolved".into(),
            Ok(Sent::Viewed { path, is_viewed }) => {
                self.mark_viewed(path, is_viewed)
            }
            Ok(Sent::Blob { path, lines }) => self.blob_loaded(&path, &lines),
            Err(failure) => {
                let status = format!("error: {}", failure.message());
                match failure {
                    Failure::Review(error) => self.reject_review(error),
                    Failure::Draft(draft, error) => {
                        self.reject_draft(draft, error);
                    }
                    Failure::Blob(path, _) => {
                        self.fetching.remove(&path);
                        self.deferred = None;
                    }
                    Failure::Other(_) => {}
                }

                status
            }
        };

        self.prune_focus();
    }

    /// GitHub has named the draft. Whatever was asked of it while it had no
    /// name — an edit, a discard — goes out now that it has one.
    fn draft_created(
        &mut self,
        draft: u64,
        review: Arc<str>,
        comment: Arc<str>,
    ) -> String {
        self.pending_review = Some(review);

        let Some(index) = self.draft_by_id(draft) else {
            return "draft saved".into();
        };

        self.drafts[index].remote = Some(comment);
        let is_deleting = self.drafts[index].sync == Sync::Deleting;
        let is_dirty = matches!(
            self.drafts[index].sync,
            Sync::Creating { is_dirty: true }
        );
        let status = if is_deleting {
            self.drafts[index].sync = Sync::Synced;
            self.discard_draft(index);
            "discarding draft…".into()
        } else if is_dirty {
            self.update_draft(index);
            "saving draft…".into()
        } else {
            self.drafts[index].sync = Sync::Synced;
            "draft saved".into()
        };

        // The review this opened is what everything still queued was waiting
        // for.
        self.create_drafts();

        status
    }

    fn draft_settled(&mut self, draft: u64) -> String {
        if let Some(index) = self.draft_by_id(draft) {
            self.drafts[index].sync = Sync::Synced;
        }

        "draft saved".into()
    }

    /// A draft the server refused stays on screen carrying the reason. Dropping
    /// it would throw away writing the user cannot get back.
    fn reject_draft(&mut self, draft: u64, error: String) {
        let Some(index) = self.draft_by_id(draft) else {
            return;
        };

        if let Some(comment) = &self.drafts[index].remote {
            self.retired.remove(comment);
        }

        self.drafts[index].sync = Sync::Failed(error);
    }

    /// A rejected review keeps everything it was made of. The summary goes back
    /// into the overlay with GitHub's reason above it, since the reason names a
    /// field and a rule and the status bar shows one line of it.
    fn reject_review(&mut self, error: String) {
        let Some(submission) = self.sending.as_mut() else {
            return;
        };

        submission.error = Some(error);

        // Reopening mid-edit would steal the keyboard from whatever the user
        // moved on to; the overlay waits for the next `s` instead.
        if self.mode == Mode::Normal && self.composer.is_none() {
            self.submission = self.sending.take();
            self.mode = Mode::Submit;
        }
    }

    /// Escape leaves the composer, but work is not thrown away on one key: a
    /// changed buffer arms first and says so, and the next escape discards it.
    pub(super) fn cancel_comment(&mut self) {
        let Some(composer) = self.composer.as_mut() else {
            self.mode = Mode::Normal;
            return;
        };

        if composer.is_dirty() && !composer.is_discard_armed {
            composer.is_discard_armed = true;
            self.status = "esc again to discard".into();
            return;
        }

        self.composer = None;
        self.mode = Mode::Normal;
        self.selection = None;
        self.status.clear();
    }

    pub(super) fn commit_comment(&mut self) {
        let Some(composer) = self.composer.take() else {
            return;
        };

        let body = composer.editor.trimmed_text();

        self.mode = Mode::Normal;
        self.selection = None;

        let saved = match composer.target {
            Target::Reply { in_reply_to } => {
                if body.is_empty() {
                    self.status = "empty reply discarded".into();
                    return;
                }

                self.send(Request::Reply { in_reply_to, body });
                self.status = "sending reply…".into();
                None
            }
            Target::Line {
                anchor,
                rows,
                replacing,
            } => self.save_draft(
                composer.path,
                Attachment::Lines { rows, anchor },
                body,
                replacing,
            ),
            Target::File { replacing } => self.save_draft(
                composer.path,
                Attachment::File,
                body,
                replacing,
            ),
        };

        // The note that was just written takes the focus, so it can be read
        // back, reopened, or thrown away without hunting for it. A file note
        // has no line to leave the cursor on, which is what makes this the only
        // way back to one.
        self.set_focus(saved.map(Card::Draft));
    }

    /// Files a composed body as a draft, revising `replacing` when the composer
    /// was reopened on one. Emptying a reopened draft is how it gets thrown away.
    ///
    /// The draft is on screen before GitHub has been told about it, so writing
    /// one is two steps: the local copy, then the request that catches the
    /// server up with it. Answers with the draft that now holds the body, or
    /// nothing when there is no longer one.
    fn save_draft(
        &mut self,
        path: Arc<str>,
        attachment: Attachment,
        body: String,
        replacing: Option<u64>,
    ) -> Option<u64> {
        let Some(index) = replacing.and_then(|id| self.draft_by_id(id)) else {
            if body.is_empty() {
                self.status = "empty comment discarded".into();
                return None;
            }

            let id = self.take_draft_id();
            self.drafts.push(Draft {
                id,
                path,
                attachment,
                body,
                remote: None,
                sync: Sync::Queued,
            });
            self.status = "saving draft…".into();
            self.create_drafts();
            return Some(id);
        };

        if body.is_empty() {
            self.discard_draft(index);
            return None;
        }

        let draft = &mut self.drafts[index];
        let id = draft.id;
        draft.body = body;

        match draft.sync {
            // Nothing has left yet, so the new body is simply what gets sent.
            Sync::Queued => self.status = "saving draft…".into(),
            // An edit that beats its own creation home has nothing to address
            // itself to, so it rides along on that answer instead.
            Sync::Creating { .. } => {
                draft.sync = Sync::Creating { is_dirty: true };
                self.status = "saving draft…".into();
            }
            _ => self.update_draft(index),
        }

        Some(id)
    }

    /// Sends the body of an already-created draft. A draft with no comment id
    /// has nothing to send it to, which only happens after a creation failed.
    fn update_draft(&mut self, index: usize) {
        let draft = &mut self.drafts[index];
        let Some(comment) = draft.remote.clone() else {
            draft.sync = Sync::Queued;
            self.create_drafts();
            return;
        };

        let (id, body) = (draft.id, draft.body.clone());
        draft.sync = Sync::Updating;
        self.send(Request::UpdateComment {
            draft: id,
            comment,
            body,
        });
        self.status = "saving draft…".into();
    }

    /// Sends every draft GitHub has not been told about yet.
    ///
    /// The first one has to open the pending review, and a second sent beside
    /// it would open a second review, so nothing else leaves until that answer
    /// names the review the rest can join.
    pub(super) fn create_drafts(&mut self) {
        let Some(pull_request) = self.pr.as_ref().map(|pr| pr.id.clone())
        else {
            return;
        };

        let parent = match &self.pending_review {
            Some(review) => Parent::Review(review.clone()),
            None if self.is_draft_in_flight() => return,
            None => Parent::PullRequest(pull_request),
        };

        let queued = self
            .drafts
            .iter()
            .filter(|draft| draft.sync == Sync::Queued)
            .count();

        let is_opening = matches!(parent, Parent::PullRequest(_));
        let limit = if is_opening { queued.min(1) } else { queued };
        self.effects.reserve(limit);

        let mut sent = 0;
        for draft in self
            .drafts
            .iter_mut()
            .filter(|draft| draft.sync == Sync::Queued)
            .take(limit)
        {
            let request = Request::AddThread {
                draft: draft.id,
                thread: draft.new_thread(parent.clone()),
            };

            draft.sync = Sync::Creating { is_dirty: false };
            self.effects.push(Effect::Request(request));
            sent += 1;
        }
        self.in_flight += sent;
    }

    fn is_draft_in_flight(&self) -> bool {
        self.drafts.iter().any(|draft| draft.sync.is_in_flight())
    }

    pub(super) fn draft_by_id(&self, id: u64) -> Option<usize> {
        self.drafts.iter().position(|draft| draft.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stepping_the_verdict_wraps_both_ways() {
        assert_eq!(ReviewEvent::Comment.step(-1), ReviewEvent::RequestChanges);
        assert_eq!(ReviewEvent::RequestChanges.step(1), ReviewEvent::Comment);
        assert_eq!(ReviewEvent::Comment.step(1), ReviewEvent::Approve);
        assert_eq!(ReviewEvent::Approve.step(-1), ReviewEvent::Comment);
    }
}
