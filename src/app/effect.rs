//! Work leaving the application and typed results returning to it.
//!
//! The event loop is an executor only: it drains [`Effect`] values, performs
//! them, and feeds [`Message`] values back. Fetch ordering and stale-result
//! policy stay here where they can be tested without a terminal or network.

use super::link::Errand;
use super::review::{Failure, Request, Sent};
use crate::model::{ChangedFile, Meta};
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    FetchFiles,
    FetchMeta { generation: u64 },
    ProbeOutage,
    Request(Request),
    HighlightAll,
    Highlight(Arc<str>),
    Errand(Errand),
}

#[derive(Debug)]
pub enum Message {
    Files(Result<Vec<ChangedFile>, String>),
    Meta {
        generation: u64,
        outcome: Result<Box<Meta>, String>,
    },
    Request(Result<Sent, Failure>),
    Outage(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum FilesState {
    Loading,
    Loaded,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MetaCompletion {
    Accept,
    Retry(u64),
    Ignore,
}

/// At most one metadata request is active. A write during that request marks
/// its answer stale; completion then advances the generation and asks for a
/// replacement without ever publishing the stale payload.
#[derive(Default)]
struct MetaFetch {
    generation: u64,
    is_in_flight: bool,
    is_stale: bool,
}

impl MetaFetch {
    const fn request(&mut self) -> Option<u64> {
        if self.is_in_flight {
            self.is_stale = true;
            return None;
        }

        self.generation = self.generation.wrapping_add(1);
        self.is_in_flight = true;
        Some(self.generation)
    }

    const fn invalidate(&mut self) {
        if self.is_in_flight {
            self.is_stale = true;
        }
    }

    const fn complete(&mut self, generation: u64) -> MetaCompletion {
        if !self.is_in_flight || generation != self.generation {
            return MetaCompletion::Ignore;
        }

        if self.is_stale {
            self.is_stale = false;
            self.generation = self.generation.wrapping_add(1);
            return MetaCompletion::Retry(self.generation);
        }

        self.is_in_flight = false;
        MetaCompletion::Accept
    }
}

/// Initial-load diagnostics plus the metadata request generation.
pub(super) struct Loading {
    pub files: FilesState,
    is_meta_pending: bool,
    is_started: bool,
    failure: Option<String>,
    outage: Option<String>,
    is_outage_probed: bool,
    meta: MetaFetch,
}

impl Default for Loading {
    fn default() -> Self {
        Self {
            files: FilesState::Loading,
            is_meta_pending: true,
            is_started: false,
            failure: None,
            outage: None,
            is_outage_probed: false,
            meta: MetaFetch::default(),
        }
    }
}

impl Loading {
    pub const fn is_files_pending(&self) -> bool {
        matches!(self.files, FilesState::Loading)
    }

    pub fn pending(&self) -> usize {
        usize::from(self.is_files_pending()) + usize::from(self.is_meta_pending)
    }

    pub const fn is_meta_pending(&self) -> bool {
        self.is_meta_pending
    }

    pub const fn start(&mut self) -> Option<u64> {
        if self.is_started {
            return None;
        }

        self.is_started = true;
        self.meta.request()
    }

    pub const fn request_meta(&mut self) -> Option<u64> {
        self.meta.request()
    }

    pub const fn invalidate_meta(&mut self) {
        self.meta.invalidate();
    }

    pub const fn complete_meta(&mut self, generation: u64) -> MetaCompletion {
        self.meta.complete(generation)
    }

    pub const fn meta_ready(&mut self) {
        self.is_meta_pending = false;
    }

    pub fn fail(&mut self, failure: String) -> bool {
        self.failure = Some(failure);

        if self.is_outage_probed {
            return false;
        }

        self.is_outage_probed = true;
        true
    }

    pub fn set_outage(&mut self, outage: String) {
        self.outage = Some(outage);
    }

    pub fn status(&self) -> String {
        if let Some(outage) = &self.outage {
            return outage.clone();
        }

        self.failure
            .as_ref()
            .map_or_else(String::new, |failure| format!("error: {failure}"))
    }

    pub const fn take_failure(&mut self) -> Option<String> {
        self.failure.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::App;
    use crate::model::PullRequest;
    use std::collections::HashSet;

    fn meta(title: &str) -> Meta {
        Meta {
            pr: PullRequest {
                title: title.to_owned(),
                ..PullRequest::default()
            },
            threads: Vec::new(),
            discussion: Vec::new(),
            pending_review: None,
            viewed: HashSet::new(),
        }
    }

    #[test]
    fn a_write_during_a_fetch_retries_without_accepting_the_stale_result() {
        let mut fetch = MetaFetch::default();
        let first = fetch.request().unwrap();

        assert_eq!(fetch.request(), None);
        assert_eq!(fetch.complete(first), MetaCompletion::Retry(first + 1));
        assert_eq!(fetch.complete(first), MetaCompletion::Ignore);
        assert_eq!(fetch.complete(first + 1), MetaCompletion::Accept);
    }

    #[test]
    fn an_invalidation_between_fetches_costs_no_round_trip() {
        let mut fetch = MetaFetch::default();
        fetch.invalidate();

        assert_eq!(fetch.request(), Some(1));
        assert_eq!(fetch.complete(1), MetaCompletion::Accept);
    }

    #[test]
    fn an_old_generation_cannot_finish_the_current_fetch() {
        let mut fetch = MetaFetch::default();
        let first = fetch.request().unwrap();
        fetch.invalidate();
        let second = match fetch.complete(first) {
            MetaCompletion::Retry(generation) => generation,
            other => panic!("expected a retry, got {other:?}"),
        };

        assert_eq!(fetch.complete(first), MetaCompletion::Ignore);
        assert_eq!(fetch.complete(second), MetaCompletion::Accept);
    }

    #[test]
    fn starting_the_app_queues_each_initial_read_once() {
        let mut app = App::new();

        app.start();
        assert_eq!(
            app.take_effects(),
            [Effect::FetchFiles, Effect::FetchMeta { generation: 1 }]
        );

        app.start();
        assert!(app.take_effects().is_empty());
    }

    #[test]
    fn a_stale_metadata_payload_never_reaches_application_state() {
        let mut app = App::new();
        app.start();
        app.take_effects();

        app.receive(Message::Request(Ok(Sent::Reply)));
        assert!(app.take_effects().is_empty());

        assert!(!app.receive(Message::Meta {
            generation: 1,
            outcome: Ok(Box::new(meta("stale"))),
        }));
        assert!(app.pr.is_none());
        assert_eq!(app.take_effects(), [Effect::FetchMeta { generation: 2 }]);

        assert!(app.receive(Message::Meta {
            generation: 2,
            outcome: Ok(Box::new(meta("fresh"))),
        }));
        assert_eq!(app.pr.as_ref().map(|pr| pr.title.as_str()), Some("fresh"));
    }

    #[test]
    fn a_write_between_fetches_starts_one_metadata_read() {
        let mut app = App::new();
        app.start();
        app.take_effects();
        app.receive(Message::Meta {
            generation: 1,
            outcome: Ok(Box::new(meta("first"))),
        });

        app.receive(Message::Request(Ok(Sent::Reply)));

        assert_eq!(app.take_effects(), [Effect::FetchMeta { generation: 2 }]);
    }

    #[test]
    fn a_viewed_mark_invalidates_an_active_read_without_starting_its_own() {
        let mut app = App::new();
        app.start();
        app.take_effects();

        app.receive(Message::Request(Ok(Sent::Viewed {
            path: "src/main.rs".into(),
            is_viewed: true,
        })));
        assert!(app.take_effects().is_empty());

        assert!(!app.receive(Message::Meta {
            generation: 1,
            outcome: Ok(Box::new(meta("stale"))),
        }));
        assert_eq!(app.take_effects(), [Effect::FetchMeta { generation: 2 }]);
    }

    #[test]
    fn an_initial_failure_probes_once_and_an_outage_replaces_it() {
        let mut app = App::new();
        app.start();
        app.take_effects();

        app.receive(Message::Files(Err("files failed".into())));
        assert_eq!(app.status, "error: files failed");
        assert_eq!(app.take_effects(), [Effect::ProbeOutage]);

        app.receive(Message::Meta {
            generation: 1,
            outcome: Err("metadata failed".into()),
        });
        assert!(app.take_effects().is_empty());

        app.receive(Message::Outage("github incident".into()));
        assert_eq!(app.status, "github incident");
        assert_eq!(app.take_failure().as_deref(), Some("metadata failed"));
    }
}
