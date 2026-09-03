//! The code-review host boundary.
//!
//! The application speaks only in these domain types and operations. A host
//! such as GitHub or GitLab owns authentication, URLs, pagination and wire
//! formats behind an implementation of [`Provider`].

use crate::{
    AddedThread, ChangedFile, Meta, NewThread, Parent, PullRequestList, Repo,
    ReviewEvent, Summary,
};
use anyhow::Result;
use std::{future::Future, sync::Arc};

/// Everything the application needs from a code-review host.
///
/// The returned futures stay concrete and `Send`, so generic consumers get
/// static dispatch without boxing. Select a provider once at startup, then
/// pass its concrete type into a generic runtime.
pub trait Provider: Copy + Send + 'static {
    /// Parses `[HOST/]NAMESPACE/REPOSITORY` using provider host semantics.
    fn parse_repo(self, slug: &str) -> Result<Repo>;

    /// Returns the browser URL for a pull request.
    fn pull_request_url(self, repo: &Repo, number: u32) -> String;

    /// Returns the browser URL for a comment identified by `reply_target`.
    fn comment_url(
        self,
        repo: &Repo,
        number: u32,
        reply_target: &str,
    ) -> String;

    /// Returns a permalink to a file or inclusive line range at `commit`.
    fn blob_url(
        self,
        repo: &Repo,
        commit: &str,
        path: &str,
        lines: Option<(u32, u32)>,
    ) -> String;

    /// Detects the repository in the current working directory, if any.
    fn current_repo_if_present(
        self,
    ) -> impl Future<Output = Result<Option<Repo>>> + Send;

    /// Lists open pull requests in one repository.
    fn repository_pull_requests(
        self,
        repo: Repo,
    ) -> impl Future<Output = Result<PullRequestList>> + Send;

    /// Lists open pull requests relevant to the authenticated user.
    fn user_pull_requests(
        self,
    ) -> impl Future<Output = Result<PullRequestList>> + Send;

    /// Fetches the selector summary for one pull request.
    fn fetch_summary(
        self,
        repo: &Repo,
        number: u32,
    ) -> impl Future<Output = Result<Summary>> + Send;

    /// Fetches all changed files and their parsed patches.
    fn fetch_files(
        self,
        repo: &Repo,
        number: u32,
    ) -> impl Future<Output = Result<Vec<ChangedFile>>> + Send;

    /// Fetches complete pull request metadata and review conversations.
    fn fetch_meta(
        self,
        repo: &Repo,
        number: u32,
    ) -> impl Future<Output = Result<Meta>> + Send;

    /// Fetches a file exactly as it exists at `commit`.
    fn fetch_blob(
        self,
        repo: &Repo,
        path: &str,
        commit: &str,
    ) -> impl Future<Output = Result<String>> + Send;

    /// Creates a review conversation or file-level comment.
    fn add_thread(
        self,
        repo: &Repo,
        thread: NewThread,
    ) -> impl Future<Output = Result<AddedThread>> + Send;

    /// Replaces the body of an existing comment.
    fn update_comment(
        self,
        repo: &Repo,
        comment: Arc<str>,
        body: String,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Deletes an existing comment.
    fn delete_comment(
        self,
        repo: &Repo,
        comment: Arc<str>,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Publishes a pending review or creates a review with no drafts.
    fn submit_review(
        self,
        repo: &Repo,
        parent: Parent,
        event: ReviewEvent,
        body: String,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Adds a reply beneath an existing review conversation.
    fn reply(
        self,
        repo: &Repo,
        number: u32,
        in_reply_to: Arc<str>,
        body: String,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Changes whether a review conversation is resolved.
    fn set_resolved(
        self,
        repo: &Repo,
        thread_id: Arc<str>,
        is_resolved: bool,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Changes whether the current viewer has marked a file as viewed.
    fn set_viewed(
        self,
        repo: &Repo,
        pr: Arc<str>,
        path: &str,
        is_viewed: bool,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Describes a provider outage relevant to reviews, when one exists.
    fn fetch_outage(
        self,
        repo: &Repo,
    ) -> impl Future<Output = Option<String>> + Send;
}
