//! GitLab client and API operations.

#![deny(missing_docs)]

mod detection;
mod ids;
mod project;
mod transport;
mod url;
mod wire;

use anyhow::{Context, Result, bail};
use prtui_core::{
    AddedThread, ChangedFile, Comment, Meta, NewThread, Parent, Provider,
    PullRequestList, PullRequestListItem, PullRequestListScope,
    PullRequestOverview, PullRequestTarget, Repo, ReviewEvent, ReviewStatus,
    ReviewThread, Summary, Threads,
};
use serde::de::DeserializeOwned;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, OnceLock};

use ids::{CommentRef, ReplyRef, ThreadRef};
use project::ProjectRef;
use transport::Retry;
use url::{escape_path, escape_segment};
use wire::{
    DiffRefs, WireApprovals, WireDiff, WireDiscussion, WireDraftNote, WireJob,
    WireMergeRequest, WirePipeline, WireReviewer, WireTreeEntry,
};

const DEFAULT_HOST: &str = "gitlab.com";
const API_LIMIT: u64 = 64 * 1024 * 1024;

const BLOB_LIMIT: u64 = 8 * 1024 * 1024;

const PAGE_SIZE: u32 = 100;

/// GitLab's implementation of the code-review provider boundary.
#[derive(Clone, Copy, Debug, Default)]
pub struct GitLab;

/// Returns the host a repository lives on.
pub fn host_of(repo: &Repo) -> &str {
    repo.host.as_deref().unwrap_or(DEFAULT_HOST)
}

fn parse_repo(slug: &str) -> Result<Repo> {
    let parts: Vec<&str> =
        slug.trim().split('/').filter(|s| !s.is_empty()).collect();

    let is_host = parts
        .first()
        .is_some_and(|first| first.contains('.') || first.contains(':'));

    let (host, path) = match (is_host, parts.as_slice()) {
        (true, [host, rest @ ..]) => (Some((*host).to_string()), rest),
        (_, rest) => (None, rest),
    };

    let [namespace @ .., name] = path else {
        bail!("expected [HOST/]NAMESPACE/PROJECT, got {slug}");
    };

    if namespace.is_empty() {
        bail!("expected [HOST/]NAMESPACE/PROJECT, got {slug}");
    }

    Ok(Repo {
        host,
        namespace: namespace.join("/"),
        name: (*name).to_string(),
    })
}

fn api_with(repo: &Repo, id: &ProjectRef, path: &str) -> String {
    format!("https://{}/api/v4/projects/{id}{path}", host_of(repo))
}

async fn api(repo: &Repo, path: &str) -> Result<String> {
    let id = project::reference(repo).await?;

    Ok(api_with(repo, &id, path))
}

fn web_url(repo: &Repo) -> String {
    format!("https://{}/{}/{}", host_of(repo), repo.namespace, repo.name)
}

struct ReviewDiff {
    refs: DiffRefs,
    files: Vec<WireDiff>,
}

fn diff_cache() -> &'static Mutex<HashMap<String, Arc<ReviewDiff>>> {
    static DIFFS: OnceLock<Mutex<HashMap<String, Arc<ReviewDiff>>>> =
        OnceLock::new();

    DIFFS.get_or_init(Mutex::default)
}

async fn review_diff(repo: &Repo, number: u32) -> Result<Arc<ReviewDiff>> {
    let key = format!("{}#{number}", repo.slug());
    if let Ok(cache) = diff_cache().lock()
        && let Some(diff) = cache.get(&key)
    {
        return Ok(diff.clone());
    }

    fetch_files(repo, number).await?;
    diff_cache()
        .lock()
        .ok()
        .and_then(|cache| cache.get(&key).cloned())
        .context("merge request diff is unavailable; reload and try again")
}

async fn read_bytes(
    repo: &Repo,
    url: String,
    what: &'static str,
    limit: u64,
) -> Result<Vec<u8>> {
    let token = transport::token(host_of(repo)).await;

    tokio::task::spawn_blocking(move || {
        let mut response = transport::get(&url, token.as_deref())?;
        transport::check(&mut response, what)?;

        response
            .body_mut()
            .with_config()
            .limit(limit)
            .read_to_vec()
            .with_context(|| format!("failed to read {what}"))
    })
    .await
    .context("request task failed")?
}

async fn read<T: DeserializeOwned>(
    repo: &Repo,
    path: &str,
    what: &'static str,
) -> Result<T> {
    let bytes =
        read_bytes(repo, api(repo, path).await?, what, API_LIMIT).await?;

    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {what}"))
}

async fn read_all<T: DeserializeOwned + Send + 'static>(
    repo: &Repo,
    path: &str,
    what: &'static str,
) -> Result<Vec<T>> {
    read_all_url(host_of(repo), api(repo, path).await?, what).await
}

async fn read_all_url<T: DeserializeOwned + Send + 'static>(
    host: &str,
    url: String,
    what: &'static str,
) -> Result<Vec<T>> {
    let token = transport::token(host).await;
    tokio::task::spawn_blocking(move || {
        transport::read_all(&url, token.as_deref(), what)
    })
    .await
    .context("request task failed")?
}

async fn send_json(
    repo: &Repo,
    method: Method,
    path: String,
    body: serde_json::Value,
    what: &'static str,
) -> Result<Vec<u8>> {
    let token = transport::write_token(repo).await?;
    let url = api(repo, &path).await?;

    tokio::task::spawn_blocking(move || {
        let mut response = match method {
            Method::Post => {
                transport::post(&url, &token, &body, Retry::Refusals)?
            }
            Method::Put => transport::put(&url, &token, &body)?,
            Method::Delete => transport::delete(&url, &token)?,
        };
        transport::check(&mut response, what)?;

        response
            .body_mut()
            .with_config()
            .limit(API_LIMIT)
            .read_to_vec()
            .with_context(|| format!("failed to read {what}"))
    })
    .await
    .context("request task failed")?
}

#[derive(Clone, Copy)]
enum Method {
    Post,
    Put,
    Delete,
}

async fn merge_request(repo: &Repo, number: u32) -> Result<WireMergeRequest> {
    read(repo, &format!("/merge_requests/{number}"), "merge request").await
}

async fn repository_pull_requests(repo: Repo) -> Result<PullRequestList> {
    let wire: Vec<WireMergeRequest> = read_all(
        &repo,
        "/merge_requests?state=opened&order_by=updated_at",
        "merge requests",
    )
    .await?;

    let repo = Arc::new(repo);
    let items = wire
        .into_iter()
        .map(|mr| list_item(Arc::clone(&repo), mr))
        .collect();

    Ok(PullRequestList {
        scope: PullRequestListScope::Repository,
        items,
    })
}

async fn user_pull_requests() -> Result<PullRequestList> {
    let host = std::env::var("GITLAB_HOST")
        .ok()
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| DEFAULT_HOST.to_owned());
    let wire: Vec<WireMergeRequest> = read_all_url(
        &host,
        format!("https://{host}/api/v4/merge_requests?scope=created_by_me&state=opened&order_by=updated_at"),
        "merge requests",
    ).await?;

    let items = wire
        .into_iter()
        .map(|mr| {
            let path = mr
                .references
                .as_ref()
                .and_then(|refs| refs.full.split_once('!'))
                .map(|(path, _)| path)
                .context("merge request is missing its project reference")?;
            let (namespace, name) = path
                .rsplit_once('/')
                .context("merge request has an invalid project reference")?;
            let repo = Repo {
                host: Some(host.clone()),
                namespace: namespace.to_owned(),
                name: name.to_owned(),
            };

            Ok(list_item(Arc::new(repo), mr))
        })
        .collect::<Result<Vec<_>>>()?;

    Ok(PullRequestList {
        scope: PullRequestListScope::User,
        items,
    })
}

fn list_item(repo: Arc<Repo>, mr: WireMergeRequest) -> PullRequestListItem {
    let review_status = if mr.draft {
        ReviewStatus::Draft
    } else if mr.reviewers.is_empty() {
        ReviewStatus::NoDecision
    } else {
        ReviewStatus::ReviewRequired
    };

    PullRequestListItem {
        target: PullRequestTarget {
            repo,
            number: mr.iid,
        },
        title: mr.title,
        author: mr
            .author
            .as_ref()
            .map(wire::WireUser::display)
            .unwrap_or_default(),
        review_status,
    }
}

async fn fetch_summary(repo: &Repo, number: u32) -> Result<Summary> {
    Ok(fetch_overview(repo, number).await?.summary)
}

async fn fetch_overview(
    repo: &Repo,
    number: u32,
) -> Result<PullRequestOverview> {
    let path = format!("/merge_requests/{number}/discussions");
    let (mr, files, checks, approvals, discussions, reviewers) = tokio::try_join!(
        merge_request(repo, number),
        fetch_diffs(repo, number),
        fetch_checks(repo, number),
        fetch_approvals(repo, number),
        read_all::<WireDiscussion>(repo, &path, "discussions"),
        fetch_reviewers(repo, number),
    )?;

    let files = wire::changed_files(&files);
    let head = mr.diff_refs.as_ref().map(|refs| refs.head_sha.clone());
    let (threads, conversation) =
        wire::split_discussions(discussions, number, head.as_deref());
    let unresolved =
        threads.iter().filter(|thread| !thread.is_resolved).count() as u32;

    let summary = Summary {
        author: mr
            .author
            .as_ref()
            .map(wire::WireUser::display)
            .unwrap_or_default(),
        base_ref: mr.target_branch.clone(),
        head_ref: mr.source_branch.clone(),
        additions: files.iter().map(|file| file.additions).sum(),
        deletions: files.iter().map(|file| file.deletions).sum(),
        changed_files: files.len() as u32,
        updated_on: mr.updated_at.clone(),
        comments: conversation.len() as u32,
        checks,
        reviewers: wire::reviewers(&reviewers, &approvals),
        threads: Threads {
            unresolved,
            total: threads.len() as u32,
            is_truncated: false,
        },
    };

    Ok(PullRequestOverview {
        summary,
        body: mr.description.unwrap_or_default(),
        discussion: conversation,
    })
}

async fn fetch_checks(
    repo: &Repo,
    number: u32,
) -> Result<Vec<prtui_core::Check>> {
    let pipelines: Vec<WirePipeline> = read_all(
        repo,
        &format!("/merge_requests/{number}/pipelines"),
        "pipelines",
    )
    .await?;

    let Some(latest) = pipelines.into_iter().next() else {
        return Ok(Vec::new());
    };

    let jobs: Vec<WireJob> =
        read_all(repo, &format!("/pipelines/{}/jobs", latest.id), "jobs")
            .await?;

    Ok(jobs.into_iter().map(WireJob::into_check).collect())
}

async fn fetch_reviewers(
    repo: &Repo,
    number: u32,
) -> Result<Vec<WireReviewer>> {
    read_all(
        repo,
        &format!("/merge_requests/{number}/reviewers"),
        "reviewers",
    )
    .await
}

async fn current_user(repo: &Repo) -> Result<wire::WireUser> {
    let bytes = read_bytes(
        repo,
        format!("https://{}/api/v4/user", host_of(repo)),
        "current user",
        API_LIMIT,
    )
    .await?;
    serde_json::from_slice(&bytes).context("failed to parse current user")
}

async fn fetch_approvals(repo: &Repo, number: u32) -> Result<WireApprovals> {
    read(
        repo,
        &format!("/merge_requests/{number}/approvals"),
        "approvals",
    )
    .await
}

async fn fetch_diffs(repo: &Repo, number: u32) -> Result<Vec<WireDiff>> {
    read_all(repo, &format!("/merge_requests/{number}/diffs"), "diffs").await
}

async fn fetch_files(repo: &Repo, number: u32) -> Result<Vec<ChangedFile>> {
    let refs = merge_request(repo, number)
        .await?
        .diff_refs
        .context("GitLab is still preparing this merge request's diff")?;
    let diffs = fetch_diffs(repo, number).await?;
    let current = merge_request(repo, number).await?.diff_refs;
    if current.as_ref() != Some(&refs) {
        bail!(
            "merge request changed while loading the diff; reload and try again"
        );
    }

    let files = wire::changed_files(&diffs);
    if let Ok(mut cache) = diff_cache().lock() {
        cache.insert(
            format!("{}#{number}", repo.slug()),
            Arc::new(ReviewDiff { refs, files: diffs }),
        );
    }

    Ok(files)
}

async fn fetch_meta(repo: &Repo, number: u32) -> Result<Meta> {
    let discussions_path = format!("/merge_requests/{number}/discussions");
    let drafts_path = format!("/merge_requests/{number}/draft_notes");
    let (mr, discussions, drafts) = tokio::try_join!(
        merge_request(repo, number),
        read_all::<WireDiscussion>(repo, &discussions_path, "discussions"),
        read_all::<WireDraftNote>(repo, &drafts_path, "draft notes"),
    )?;

    let viewer = if drafts.is_empty() {
        None
    } else {
        Some(current_user(repo).await?)
    };

    meta(mr, discussions, drafts, viewer.as_ref())
}

fn meta(
    mr: WireMergeRequest,
    discussions: Vec<WireDiscussion>,
    drafts: Vec<WireDraftNote>,
    viewer: Option<&wire::WireUser>,
) -> Result<Meta> {
    let number = mr.iid;
    let head = mr.diff_refs.as_ref().map(|refs| refs.head_sha.clone());
    let author = if drafts.is_empty() {
        String::new()
    } else {
        viewer
            .context("current user is required to identify draft authors")?
            .display()
    };
    let (mut threads, discussion): (Vec<ReviewThread>, Vec<Comment>) =
        wire::split_discussions(discussions, number, head.as_deref());

    let has_drafts = !drafts.is_empty();
    let mut discussion = discussion;
    for draft in drafts {
        match draft.into_pending(number, &author) {
            wire::Pending::Thread(thread) => threads.push(thread),
            wire::Pending::Discussion(comment) => discussion.push(comment),
        }
    }

    Ok(Meta {
        pr: mr.into_pull_request(),
        threads,
        discussion,
        // GitLab publishes drafts by merge request, without a review object.
        pending_review: has_drafts
            .then(|| Arc::from(number.to_string()) as Arc<str>),
        // Per-file viewed state is not exposed by the API.
        viewed: HashSet::new(),
    })
}

async fn fetch_blob(repo: &Repo, path: &str, commit: &str) -> Result<String> {
    let id = project::reference(repo).await?;

    let url = if id.rewrites_separators() {
        api_with(
            repo,
            &id,
            &format!(
                "/repository/blobs/{}/raw",
                blob_sha(repo, path, commit).await?
            ),
        )
    } else {
        api_with(
            repo,
            &id,
            &format!(
                "/repository/files/{}/raw?ref={}",
                escape_segment(path),
                escape_segment(commit)
            ),
        )
    };

    let bytes = read_bytes(repo, url, "file contents", BLOB_LIMIT).await?;

    String::from_utf8(bytes).context("file is not valid UTF-8")
}

async fn blob_sha(repo: &Repo, path: &str, commit: &str) -> Result<String> {
    let directory = path.rsplit_once('/').map_or("", |(dir, _)| dir);
    let listing = format!(
        "/repository/tree?path={}&ref={}",
        escape_segment(directory),
        escape_segment(commit)
    );

    let entries: Vec<WireTreeEntry> =
        read_all(repo, &listing, "repository tree").await?;

    entries
        .into_iter()
        .find(|entry| entry.path == path)
        .map(|entry| entry.id)
        .with_context(|| format!("{path} does not exist at {commit}"))
}

async fn add_thread(repo: &Repo, thread: NewThread) -> Result<AddedThread> {
    let number = parent_number(&thread.parent)?;
    let diff = review_diff(repo, number).await?;
    let file = diff
        .files
        .iter()
        .find(|file| file.new_path == thread.path.as_ref())
        .context(
            "file is missing from the reviewed diff; reload and try again",
        )?;

    let body = serde_json::json!({
        "note": thread.body,
        "position": wire::position(&diff.refs, file, thread.anchor)?,
    });

    let bytes = send_json(
        repo,
        Method::Post,
        format!("/merge_requests/{number}/draft_notes"),
        body,
        "draft comment",
    )
    .await?;

    let draft: WireDraftNote = serde_json::from_slice(&bytes)
        .context("failed to parse draft comment")?;

    Ok(AddedThread {
        review: Arc::from(number.to_string()),
        comment: Arc::from(
            CommentRef::Draft {
                mr: number,
                draft: draft.id,
            }
            .to_string(),
        ),
    })
}

fn parent_number(parent: &Parent) -> Result<u32> {
    let id = match parent {
        Parent::Review(id) | Parent::PullRequest(id) => id,
    };

    id.parse()
        .with_context(|| format!("malformed GitLab merge request id {id:?}"))
}

async fn update_comment(
    repo: &Repo,
    comment: Arc<str>,
    body: String,
) -> Result<()> {
    let (path, body) = match CommentRef::parse(&comment)? {
        CommentRef::Published { mr, note } => (
            format!("/merge_requests/{mr}/notes/{note}"),
            serde_json::json!({ "body": body }),
        ),
        CommentRef::Draft { mr, draft } => {
            let path = format!("/merge_requests/{mr}/draft_notes/{draft}");

            (path.clone(), draft_update(repo, &path, body).await?)
        }
    };

    send_json(repo, Method::Put, path, body, "comment update").await?;

    Ok(())
}

// Omitting position on an update can detach a draft from its diff.
async fn draft_update(
    repo: &Repo,
    path: &str,
    body: String,
) -> Result<serde_json::Value> {
    let existing: serde_json::Value = read(repo, path, "draft comment").await?;
    let mut update = serde_json::json!({ "note": body });

    let anchored = existing.get("position").filter(|position| {
        !position["new_path"].is_null() || !position["old_path"].is_null()
    });

    if let Some(position) = anchored {
        update["position"] = position.clone();
    }

    Ok(update)
}

async fn delete_comment(repo: &Repo, comment: Arc<str>) -> Result<()> {
    let path = match CommentRef::parse(&comment)? {
        CommentRef::Published { mr, note } => {
            format!("/merge_requests/{mr}/notes/{note}")
        }
        CommentRef::Draft { mr, draft } => {
            format!("/merge_requests/{mr}/draft_notes/{draft}")
        }
    };

    send_json(
        repo,
        Method::Delete,
        path,
        serde_json::Value::Null,
        "comment deletion",
    )
    .await?;

    Ok(())
}

async fn submit_review(
    repo: &Repo,
    parent: Parent,
    event: ReviewEvent,
    body: String,
) -> Result<()> {
    let number = parent_number(&parent)?;
    let viewer = if event == ReviewEvent::RequestChanges {
        Some(current_user(repo).await?)
    } else {
        None
    };

    send_json(
        repo,
        Method::Post,
        format!("/merge_requests/{number}/draft_notes/bulk_publish"),
        wire::review_submission(event),
        "review submission",
    )
    .await?;

    if !body.trim().is_empty() {
        send_json(
            repo,
            Method::Post,
            format!("/merge_requests/{number}/notes"),
            serde_json::json!({ "body": body }),
            "review summary",
        )
        .await?;
    }

    if event == ReviewEvent::Approve {
        send_json(
            repo,
            Method::Post,
            format!("/merge_requests/{number}/approve"),
            serde_json::Value::Null,
            "approval",
        )
        .await?;
    }

    if let Some(viewer) = viewer {
        let reviewers = fetch_reviewers(repo, number).await.context(
            "review comments were published, but the requested-changes state could not be verified; check GitLab before retrying",
        )?;
        wire::verify_requested_changes(&reviewers, &viewer.username)?;
    }

    Ok(())
}

async fn reply(
    repo: &Repo,
    number: u32,
    in_reply_to: Arc<str>,
    body: String,
) -> Result<()> {
    let target = ReplyRef::parse(&in_reply_to)?;

    send_json(
        repo,
        Method::Post,
        format!(
            "/merge_requests/{number}/discussions/{}/notes",
            target.discussion
        ),
        serde_json::json!({ "body": body }),
        "reply",
    )
    .await?;

    Ok(())
}

async fn set_resolved(
    repo: &Repo,
    thread_id: Arc<str>,
    is_resolved: bool,
) -> Result<()> {
    let thread = ThreadRef::parse(&thread_id)?;

    send_json(
        repo,
        Method::Put,
        format!(
            "/merge_requests/{}/discussions/{}",
            thread.mr, thread.discussion
        ),
        serde_json::json!({ "resolved": is_resolved }),
        "resolution",
    )
    .await?;

    Ok(())
}

impl Provider for GitLab {
    fn parse_repo(self, slug: &str) -> Result<Repo> {
        parse_repo(slug)
    }

    fn pull_request_url(self, repo: &Repo, number: u32) -> String {
        format!("{}/-/merge_requests/{number}", web_url(repo))
    }

    fn comment_url(
        self,
        repo: &Repo,
        number: u32,
        reply_target: &str,
    ) -> String {
        format!(
            "{}#note_{}",
            self.pull_request_url(repo, number),
            ReplyRef::note_anchor(reply_target)
        )
    }

    fn blob_url(
        self,
        repo: &Repo,
        commit: &str,
        path: &str,
        lines: Option<(u32, u32)>,
    ) -> String {
        let base =
            format!("{}/-/blob/{commit}/{}", web_url(repo), escape_path(path));

        match lines {
            None => base,
            Some((start, end)) if start == end => format!("{base}#L{start}"),
            Some((start, end)) => format!("{base}#L{start}-{end}"),
        }
    }

    async fn repository_pull_requests(
        self,
        repo: Repo,
    ) -> Result<PullRequestList> {
        repository_pull_requests(repo).await
    }

    async fn user_pull_requests(self) -> Result<PullRequestList> {
        user_pull_requests().await
    }

    async fn fetch_summary(self, repo: &Repo, number: u32) -> Result<Summary> {
        fetch_summary(repo, number).await
    }

    async fn fetch_overview(
        self,
        repo: &Repo,
        number: u32,
    ) -> Result<PullRequestOverview> {
        fetch_overview(repo, number).await
    }

    async fn fetch_files(
        self,
        repo: &Repo,
        number: u32,
    ) -> Result<Vec<ChangedFile>> {
        fetch_files(repo, number).await
    }

    async fn fetch_meta(self, repo: &Repo, number: u32) -> Result<Meta> {
        fetch_meta(repo, number).await
    }

    async fn fetch_blob(
        self,
        repo: &Repo,
        path: &str,
        commit: &str,
    ) -> Result<String> {
        fetch_blob(repo, path, commit).await
    }

    async fn add_thread(
        self,
        repo: &Repo,
        thread: NewThread,
    ) -> Result<AddedThread> {
        add_thread(repo, thread).await
    }

    async fn update_comment(
        self,
        repo: &Repo,
        comment: Arc<str>,
        body: String,
    ) -> Result<()> {
        update_comment(repo, comment, body).await
    }

    async fn delete_comment(
        self,
        repo: &Repo,
        comment: Arc<str>,
    ) -> Result<()> {
        delete_comment(repo, comment).await
    }

    async fn submit_review(
        self,
        repo: &Repo,
        parent: Parent,
        event: ReviewEvent,
        body: String,
    ) -> Result<()> {
        submit_review(repo, parent, event, body).await
    }

    async fn reply(
        self,
        repo: &Repo,
        number: u32,
        in_reply_to: Arc<str>,
        body: String,
    ) -> Result<()> {
        reply(repo, number, in_reply_to, body).await
    }

    async fn set_resolved(
        self,
        repo: &Repo,
        thread_id: Arc<str>,
        is_resolved: bool,
    ) -> Result<()> {
        set_resolved(repo, thread_id, is_resolved).await
    }

    async fn set_viewed(
        self,
        _repo: &Repo,
        _pr: Arc<str>,
        _path: &str,
        _is_viewed: bool,
    ) -> Result<()> {
        Ok(())
    }

    async fn fetch_outage(self, _repo: &Repo) -> Option<String> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitlab_is_usable_through_the_provider_boundary() {
        let provider = GitLab;
        let repo = provider.parse_repo("group/project").unwrap();

        assert_eq!(repo.slug(), "group/project");
        assert_eq!(
            provider.pull_request_url(&repo, 42),
            "https://gitlab.com/group/project/-/merge_requests/42"
        );
        assert_eq!(
            provider.blob_url(&repo, "abc123", "src/a b.rs", Some((7, 9))),
            "https://gitlab.com/group/project/-/blob/abc123/src/a%20b.rs#L7-9"
        );
    }

    #[test]
    fn a_subgroup_path_is_not_mistaken_for_a_host() {
        let nested = parse_repo("group/sub/project").unwrap();
        assert_eq!(nested.host, None);
        assert_eq!(nested.namespace, "group/sub");
        assert_eq!(nested.name, "project");

        let hosted = parse_repo("gitlab.example.com/group/project").unwrap();
        assert_eq!(hosted.host.as_deref(), Some("gitlab.example.com"));
        assert_eq!(hosted.namespace, "group");

        let deep = parse_repo("gitlab.example.com/a/b/c/d").unwrap();
        assert_eq!(deep.host.as_deref(), Some("gitlab.example.com"));
        assert_eq!(deep.namespace, "a/b/c");
        assert_eq!(deep.name, "d");

        assert!(parse_repo("project").is_err());
        assert!(parse_repo("gitlab.example.com/project").is_err());
    }

    #[test]
    fn a_project_path_rides_in_one_encoded_segment() {
        let repo = parse_repo("gitlab.example.com/group/sub/project").unwrap();
        let path = ProjectRef::Path("group%2Fsub%2Fproject".to_owned());

        assert_eq!(
            api_with(&repo, &path, "/merge_requests/1"),
            "https://gitlab.example.com/api/v4/projects/\
             group%2Fsub%2Fproject/merge_requests/1"
        );
    }

    #[test]
    fn a_normalizing_host_is_addressed_by_id() {
        let repo = parse_repo("gitlab.example.com/group/sub/project").unwrap();

        assert_eq!(
            api_with(&repo, &ProjectRef::Id(7), "/merge_requests/1"),
            "https://gitlab.example.com/api/v4/projects/7/merge_requests/1"
        );
    }
    #[test]
    fn pending_comments_belong_to_the_viewer_not_the_merge_request_author() {
        let mr = serde_json::from_value(serde_json::json!({
            "iid": 7, "title": "Fix parser", "state": "opened",
            "author": { "username": "author", "name": "MR Author" },
            "source_branch": "fix", "target_branch": "main",
            "updated_at": "2026-09-11T00:00:00Z"
        }))
        .unwrap();
        let drafts = serde_json::from_value(serde_json::json!([
            { "id": 1, "note": "General feedback", "position": null },
            { "id": 2, "note": "Inline feedback", "position": {
                "position_type": "text", "new_path": "src/main.rs", "new_line": 4
            }}
        ])).unwrap();
        let viewer = wire::WireUser {
            username: "reviewer".to_owned(),
            name: Some("Reviewer".to_owned()),
        };

        let meta = meta(mr, Vec::new(), drafts, Some(&viewer)).unwrap();
        assert_eq!(meta.pr.author, "MR Author");
        assert_eq!(meta.discussion[0].author, "Reviewer");
        assert_eq!(meta.threads[0].comments[0].author, "Reviewer");
        assert_eq!(meta.pending_review.as_deref(), Some("7"));
    }
}
