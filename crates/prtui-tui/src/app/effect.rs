//! Work leaving the application and typed results returning to it.
//!
//! The event loop is an executor only: it drains [`Effect`] values, performs
//! them, and feeds [`Message`] values back. Fetch ordering and stale-result
//! policy stay here where they can be tested without a terminal or network.

use super::link::Errand;
use super::review::{Failure, Request, Sent};
use prtui_core::{ChangedFile, CommitLog, Meta, Summary};
use std::sync::Arc;

#[derive(Debug, PartialEq, Eq)]
pub enum Effect {
    FetchFiles,
    FetchMeta {
        generation: u64,
    },
    FetchSummary {
        generation: u64,
    },
    FetchCommits {
        generation: u64,
    },
    FetchRange {
        generation: u64,
        base: Arc<str>,
        head: Arc<str>,
    },
    ProbeOutage,
    Request(Request),
    HighlightAll,
    Highlight(Arc<str>),
    Errand(Errand),
}

#[derive(Debug)]
pub enum Message {
    Local {
        files: Vec<ChangedFile>,
        blobs: std::collections::HashMap<Arc<str>, Arc<[String]>>,
        states: std::collections::HashMap<Arc<str>, super::local::LocalFile>,
    },
    Files(Result<Vec<ChangedFile>, String>),
    Meta {
        generation: u64,
        outcome: Result<Box<Meta>, String>,
    },
    Summary {
        generation: u64,
        outcome: Result<Box<Summary>, String>,
    },
    Commits {
        generation: u64,
        outcome: Result<Box<CommitLog>, String>,
    },
    Range {
        generation: u64,
        outcome: Result<Vec<ChangedFile>, String>,
    },
    Request(Result<Sent, Failure>),
    Outage(String),
    ExternalFailure(String),
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

    pub fn clear_failure(&mut self) {
        self.failure = None;
        self.outage = None;
        self.is_outage_probed = false;
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
            return format!("outage: {outage}");
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
    use prtui_core::PullRequest;
    use std::collections::{HashMap, HashSet};

    fn meta(title: &str) -> Meta {
        Meta {
            pr: PullRequest {
                title: title.to_owned(),
                ..PullRequest::default()
            },
            threads: Vec::new(),
            discussion: Vec::new(),
            reviews: Vec::new(),
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
        assert!(app.view().pr.is_none());
        assert_eq!(app.take_effects(), [Effect::FetchMeta { generation: 2 }]);

        assert!(app.receive(Message::Meta {
            generation: 2,
            outcome: Ok(Box::new(meta("fresh"))),
        }));
        assert_eq!(app.view().pr.map(|pr| pr.title.as_str()), Some("fresh"));
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

        app.receive(Message::Meta {
            generation: 2,
            outcome: Ok(Box::new(meta("fresh"))),
        });
        app.receive(Message::Request(Ok(Sent::Viewed {
            path: "src/main.rs".into(),
            is_viewed: true,
        })));
        assert!(app.take_effects().is_empty());
    }

    fn file(path: &str) -> ChangedFile {
        ChangedFile {
            path: path.into(),
            previous_path: None,
            status: "modified".into(),
            additions: 0,
            deletions: 0,
            lines: Vec::new(),
        }
    }

    #[test]
    fn refreshing_reloads_data_and_keeps_the_selected_file_after_reordering() {
        let mut app = App::new();
        app.start();
        app.take_effects();
        app.receive(Message::Files(Ok(vec![file("a"), file("b")])));
        app.receive(Message::Meta {
            generation: 1,
            outcome: Ok(Box::new(meta("first"))),
        });
        app.set_selected_file(1, false);
        app.take_effects();

        app.refresh();
        assert_eq!(
            app.take_effects(),
            [
                Effect::FetchFiles,
                Effect::FetchMeta { generation: 2 },
                Effect::FetchSummary { generation: 1 },
                Effect::FetchCommits { generation: 1 },
            ]
        );
        app.refresh();
        assert!(app.take_effects().is_empty());
        assert_eq!(app.current_path(), Some("b"));

        app.receive(Message::Files(Ok(vec![file("b"), file("a")])));
        assert_eq!(app.current_path(), Some("b"));
        app.refresh();
        app.receive(Message::Files(Ok(vec![])));
        assert_eq!(app.current_path(), None);
    }

    #[test]
    fn a_failed_refresh_keeps_the_diff_and_allows_retrying() {
        let mut app = App::new();
        app.set_files(vec![file("a")]);
        app.refresh();
        app.take_effects();
        app.receive(Message::Files(Err("offline".into())));
        assert_eq!(app.current_path(), Some("a"));
        assert!(app.view().status.contains("offline"));
        assert!(app.take_failure().is_none());

        app.refresh();
        assert!(app.take_effects().contains(&Effect::FetchFiles));
        app.receive(Message::Files(Ok(vec![file("b")])));
        assert_eq!(app.current_path(), Some("b"));
    }

    #[test]
    fn submitted_reviews_refresh_metadata_summary_and_commits() {
        let mut app = App::new();
        app.receive(Message::Request(Ok(Sent::Review)));
        let effects = app.take_effects();
        assert!(effects.contains(&Effect::FetchMeta { generation: 1 }));
        assert!(effects.contains(&Effect::FetchSummary { generation: 1 }));
        assert!(effects.contains(&Effect::FetchCommits { generation: 1 }));
    }

    #[test]
    fn leaving_waits_for_the_write_and_a_failure_keeps_the_review_open() {
        for is_success in [true, false] {
            let mut app = App::new();
            app.runtime.in_flight = 1;
            let layout = crate::layout::Layout::compute(
                ratatui::layout::Rect::new(0, 0, 100, 30),
                app.view(),
            );
            app.apply(&crate::app::action::Action::Quit, &layout);
            assert!(!app.should_quit());
            app.receive(Message::Request(if is_success {
                Ok(Sent::Review)
            } else {
                Err(Failure::Review("rejected".into()))
            }));
            assert_eq!(app.should_quit(), is_success);
        }
    }

    #[test]
    fn local_refresh_only_requests_a_new_snapshot() {
        let mut app = App::local(
            crate::renderer::Theme::dark(),
            "/repo".into(),
            vec![file("a")],
            HashMap::default(),
            HashMap::default(),
        );
        app.take_effects();
        app.refresh();
        assert_eq!(app.take_effects(), [Effect::FetchFiles]);
        app.receive(Message::Local {
            files: vec![file("b")],
            blobs: HashMap::default(),
            states: HashMap::default(),
        });
        assert_eq!(app.current_path(), Some("b"));
        assert!(!app.runtime.is_refreshing);
    }

    #[test]
    fn an_initial_failure_probes_once_and_an_outage_replaces_it() {
        let mut app = App::new();
        app.start();
        app.take_effects();

        app.receive(Message::Files(Err("files failed".into())));
        assert_eq!(app.view().status, "error: files failed");
        assert_eq!(app.take_effects(), [Effect::ProbeOutage]);

        app.receive(Message::Meta {
            generation: 1,
            outcome: Err("metadata failed".into()),
        });
        assert!(app.take_effects().is_empty());

        app.receive(Message::Outage("provider incident".into()));
        assert_eq!(app.view().status, "outage: provider incident");
        assert_eq!(app.take_failure().as_deref(), Some("metadata failed"));
    }
}
