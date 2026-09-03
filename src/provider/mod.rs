//! The code-review host boundary.
//!
//! The application speaks only in these domain types and operations. A host
//! such as GitHub or GitLab owns authentication, URLs, pagination and wire
//! formats behind an implementation of [`Provider`].

use crate::model::{
    AddedThread, ChangedFile, Meta, NewThread, Parent, PullRequestList, Repo,
    ReviewEvent, Summary,
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
