//! The code-review host boundary.
//!
//! The application speaks only in these domain types and operations. A host
//! such as GitHub or GitLab owns authentication, URLs, pagination and wire
//! formats behind an implementation of [`Provider`].

use crate::model::{
    AddedThread, ChangedFile, Meta, NewThread, Parent, ReviewEvent,
};
use anyhow::Result;
use std::{future::Future, sync::Arc};

pub mod github;

/// Everything the application needs from a code-review host.
///
/// The returned futures stay concrete and `Send`, so generic consumers get
/// static dispatch without boxing. A runtime-selected host can be represented
/// by an enum that implements this trait at the selection boundary.
pub trait Provider: Copy + Send + 'static {
    fn parse_repo(self, slug: &str) -> Result<Repo>;

    fn repo_url(self, repo: &Repo) -> String;

    fn current_repo_if_present(
        self,
    ) -> impl Future<Output = Result<Option<Repo>>> + Send;

    fn repository_pull_requests(
        self,
        repo: Repo,
    ) -> impl Future<Output = Result<PullRequestList>> + Send;

    fn user_pull_requests(
        self,
    ) -> impl Future<Output = Result<PullRequestList>> + Send;

    fn fetch_summary(
        self,
        repo: &Repo,
        number: u32,
    ) -> impl Future<Output = Result<Summary>> + Send;

    fn fetch_files(
        self,
        repo: &Repo,
        number: u32,
    ) -> impl Future<Output = Result<Vec<ChangedFile>>> + Send;

    fn fetch_meta(
        self,
        repo: &Repo,
        number: u32,
    ) -> impl Future<Output = Result<Meta>> + Send;

    fn fetch_blob(
        self,
        repo: &Repo,
        path: &str,
        commit: &str,
    ) -> impl Future<Output = Result<String>> + Send;

    fn add_thread(
        self,
        repo: &Repo,
        thread: NewThread,
    ) -> impl Future<Output = Result<AddedThread>> + Send;

    fn update_comment(
        self,
        repo: &Repo,
        comment: Arc<str>,
        body: String,
    ) -> impl Future<Output = Result<()>> + Send;

    fn delete_comment(
        self,
        repo: &Repo,
        comment: Arc<str>,
    ) -> impl Future<Output = Result<()>> + Send;

    fn submit_review(
        self,
        repo: &Repo,
        parent: Parent,
        event: ReviewEvent,
        body: String,
    ) -> impl Future<Output = Result<()>> + Send;

    fn reply(
        self,
        repo: &Repo,
        number: u32,
        in_reply_to: u64,
        body: String,
    ) -> impl Future<Output = Result<()>> + Send;

    fn set_resolved(
        self,
        repo: &Repo,
        thread_id: Arc<str>,
        is_resolved: bool,
    ) -> impl Future<Output = Result<()>> + Send;

    fn set_viewed(
        self,
        repo: &Repo,
        pr: Arc<str>,
        path: &str,
        is_viewed: bool,
    ) -> impl Future<Output = Result<()>> + Send;

    fn fetch_outage(
        self,
        repo: &Repo,
    ) -> impl Future<Output = Option<String>> + Send;
}

#[derive(Clone)]
pub struct Repo {
    pub host: Option<String>,
    /// The account or group path that contains the repository.
    ///
    /// Providers with nested groups can keep the full path here.
    pub namespace: String,
    pub name: String,
}

impl Repo {
    pub fn slug(&self) -> String {
        match &self.host {
            Some(host) => format!("{host}/{}/{}", self.namespace, self.name),
            None => format!("{}/{}", self.namespace, self.name),
        }
    }
}

pub struct PullRequest {
    pub number: u32,
    pub title: String,
    pub review_status: ReviewStatus,
}

pub struct LocatedPullRequest {
    pub repo: Repo,
    pub pull: PullRequest,
}

/// Keeps repository scope and rows together so one local repository is owned
/// once rather than cloned into every result.
pub enum PullRequestList {
    Repository { repo: Repo, pulls: Vec<PullRequest> },
    User { pulls: Vec<LocatedPullRequest> },
}

pub struct PullRequestRow<'a> {
    pub repository: Option<&'a Repo>,
    pub number: u32,
    pub title: &'a str,
    pub review_status: &'a ReviewStatus,
}

pub struct PullRequestTarget {
    pub repo: Repo,
    pub number: u32,
}

/// What one pull request looks like from outside: enough to decide whether it
/// is worth opening, and nothing that has to be read line by line.
pub struct Summary {
    pub author: String,
    pub base_ref: String,
    pub head_ref: String,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    /// The day it last moved, which is the date half of the API timestamp.
    pub updated_on: String,
    pub comments: u32,
    /// Most blocking first: what is failing, then what is still running.
    pub checks: Vec<Check>,
    /// Most blocking first as well: changes requested, then who is still
    /// being waited on.
    pub reviewers: Vec<Reviewer>,
    pub threads: Threads,
}

/// One check on the head commit, whichever app reported it.
pub struct Check {
    pub name: String,
    pub state: CheckState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum CheckState {
    /// Ordered by how much it wants a reader's attention, which is the order
    /// the panel lists them in.
    Failed,
    Running,
    Passed,
    /// Skipped and neutral runs, which decide nothing either way.
    Skipped,
}

/// One reviewer and where they stand: someone who has answered, or someone
/// the pull request is still waiting on.
pub struct Reviewer {
    /// A login, or `owner/team` for a team that was asked as a team.
    pub name: String,
    pub is_team: bool,
    pub verdict: Verdict,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Verdict {
    ChangesRequested,
    Waiting,
    Commented,
    Approved,
}

impl Verdict {
    pub const fn label(self) -> &'static str {
        match self {
            Self::ChangesRequested => "changes requested",
            Self::Waiting => "waiting",
            Self::Commented => "commented",
            Self::Approved => "approved",
        }
    }
}

pub struct Threads {
    pub unresolved: u32,
    pub total: u32,
    /// Whether the review holds more threads than the one page that was
    /// counted, which makes `unresolved` a floor rather than the whole tally.
    pub is_truncated: bool,
}

impl PullRequestList {
    pub const fn len(&self) -> usize {
        match self {
            Self::Repository { pulls, .. } => pulls.len(),
            Self::User { pulls } => pulls.len(),
        }
    }

    pub const fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub const fn shows_repositories(&self) -> bool {
        matches!(self, Self::User { .. })
    }

    pub fn row(&self, index: usize) -> Option<PullRequestRow<'_>> {
        match self {
            Self::Repository { pulls, .. } => {
                pulls.get(index).map(|pull| pull.row(None))
            }
            Self::User { pulls } => pulls
                .get(index)
                .map(|located| located.pull.row(Some(&located.repo))),
        }
    }

    /// The same choice `select` returns, without consuming the list, which is
    /// what the summary panel asks for while the reader is still browsing.
    pub fn target(&self, index: usize) -> Option<PullRequestTarget> {
        match self {
            Self::Repository { repo, pulls } => Some(PullRequestTarget {
                repo: repo.clone(),
                number: pulls.get(index)?.number,
            }),
            Self::User { pulls } => {
                let located = pulls.get(index)?;
                Some(PullRequestTarget {
                    repo: located.repo.clone(),
                    number: located.pull.number,
                })
            }
        }
    }

    pub fn select(self, index: usize) -> Option<PullRequestTarget> {
        match self {
            Self::Repository { repo, pulls } => {
                let number = pulls.get(index)?.number;
                Some(PullRequestTarget { repo, number })
            }
            Self::User { mut pulls } => {
                if index >= pulls.len() {
                    return None;
                }
                let selected = pulls.swap_remove(index);
                Some(PullRequestTarget {
                    repo: selected.repo,
                    number: selected.pull.number,
                })
            }
        }
    }
}

impl PullRequest {
    fn row<'a>(&'a self, repository: Option<&'a Repo>) -> PullRequestRow<'a> {
        PullRequestRow {
            repository,
            number: self.number,
            title: &self.title,
            review_status: &self.review_status,
        }
    }
}

pub enum ReviewStatus {
    Draft,
    ChangesRequested,
    ReviewRequired,
    Approved,
    NoDecision,
}
