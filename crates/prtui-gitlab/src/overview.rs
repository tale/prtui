use anyhow::{Context, Result, bail};
use prtui_core::{Comment, PullRequestOverview, Repo, Summary, Threads};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ids::{CommentRef, ReplyRef};
use crate::wire::{
    WireApproval, WireApprovals, WireJob, WireReviewer, WireUser,
};
use crate::{host_of, transport, wire};

const PAGE_SIZE: u32 = 50;
const MR_PATH: &str = "/data/project/mergeRequest";
const PAGE_INFO: &str = "pageInfo { hasNextPage endCursor }";
const REVIEWERS: &str =
    "nodes { username name mergeRequestInteraction { reviewState } }";
const APPROVALS: &str = "nodes { username name }";
const JOBS: &str = "nodes { name status }";
const NOTES: &str = r"
  nodes {
    id body createdAt system resolved
    author { username name }
    position { positionType }
    discussion { id }
  }
";

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Connection<T> {
    nodes: Vec<T>,
    page_info: PageInfo,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PageInfo {
    has_next_page: bool,
    end_cursor: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MergeRequest {
    author: Option<WireUser>,
    description: Option<String>,
    source_branch: String,
    target_branch: String,
    updated_at: String,
    diff_stats_summary: Option<DiffStats>,
    reviewers: Connection<Reviewer>,
    approved_by: Connection<WireUser>,
    pipelines: Pipelines,
    notes: Connection<Note>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct DiffStats {
    additions: u32,
    deletions: u32,
    file_count: u32,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Reviewer {
    #[serde(flatten)]
    user: WireUser,
    merge_request_interaction: Option<Interaction>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Interaction {
    review_state: Option<String>,
}

#[derive(Deserialize)]
struct Pipelines {
    nodes: Vec<Pipeline>,
}

#[derive(Deserialize)]
struct Pipeline {
    id: String,
    project: Project,
    jobs: Connection<WireJob>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Project {
    full_path: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Note {
    id: String,
    body: String,
    created_at: String,
    author: Option<WireUser>,
    system: bool,
    resolved: bool,
    position: Option<Value>,
    discussion: Discussion,
}

#[derive(Deserialize)]
struct Discussion {
    id: String,
}

fn query(selection: &str) -> String {
    format!(
        r"
query($path: ID!, $iid: String!, $first: Int!, $after: String) {{
  project(fullPath: $path) {{
    mergeRequest(iid: $iid) {{ {selection} }}
  }}
}}"
    )
}

fn connection(field: &str, args: &str, selection: &str) -> String {
    format!(
        "{field}(first: $first, after: $after{args}) {{ {selection} {PAGE_INFO} }}"
    )
}

fn overview_query() -> String {
    query(&format!(
        r"
      description sourceBranch targetBranch updatedAt
      author {{ username name }}
      diffStatsSummary {{ additions deletions fileCount }}
      {}
      {}
      pipelines(first: 1) {{ nodes {{ id project {{ fullPath }} {} }} }}
      {}
    ",
        connection("reviewers", "", REVIEWERS),
        connection("approvedBy", "", APPROVALS),
        connection("jobs", ", retried: false, jobKind: BUILD", JOBS),
        connection("notes", ", filter: ONLY_COMMENTS", NOTES),
    ))
}

pub async fn fetch(repo: &Repo, number: u32) -> Result<PullRequestOverview> {
    let token = transport::token(host_of(repo)).await;
    let url = format!("https://{}/api/graphql", host_of(repo));
    let variables = json!({
        "path": format!("{}/{}", repo.namespace, repo.name),
        "iid": number.to_string(), "first": PAGE_SIZE, "after": null,
    });

    tokio::task::spawn_blocking(move || {
        read(&url, token.as_deref(), variables, number)
    })
    .await
    .context("request task failed")?
}

fn read(
    url: &str,
    token: Option<&str>,
    variables: Value,
    number: u32,
) -> Result<PullRequestOverview> {
    let mr: MergeRequest =
        request(url, token, &overview_query(), &variables, MR_PATH)?;
    let stats = mr
        .diff_stats_summary
        .context("GitLab is still preparing this merge request's diff")?;
    let reviewers = complete(
        url,
        token,
        mr.reviewers,
        &query(&connection("reviewers", "", REVIEWERS)),
        variables.clone(),
        &format!("{MR_PATH}/reviewers"),
    )?;
    let approvals = complete(
        url,
        token,
        mr.approved_by,
        &query(&connection("approvedBy", "", APPROVALS)),
        variables.clone(),
        &format!("{MR_PATH}/approvedBy"),
    )?;
    let notes = complete(
        url,
        token,
        mr.notes,
        &query(&connection("notes", ", filter: ONLY_COMMENTS", NOTES)),
        variables,
        &format!("{MR_PATH}/notes"),
    )?;
    let checks = if let Some(pipeline) = mr.pipelines.nodes.into_iter().next() {
        let query = format!(
            r"
query($path: ID!, $id: CiPipelineID!, $first: Int!, $after: String) {{
  project(fullPath: $path) {{ pipeline(id: $id) {{ {} }} }}
}}",
            connection("jobs", ", retried: false, jobKind: BUILD", JOBS)
        );
        let variables = json!({ "path": pipeline.project.full_path, "id": pipeline.id, "first": PAGE_SIZE });
        let jobs = complete(
            url,
            token,
            pipeline.jobs,
            &query,
            variables,
            "/data/project/pipeline/jobs",
        )?;
        jobs.into_iter()
            .map(|mut job| {
                job.status.make_ascii_lowercase();
                job.into_check()
            })
            .collect()
    } else {
        Vec::new()
    };

    let requested: Vec<_> = reviewers
        .into_iter()
        .map(|reviewer| WireReviewer {
            user: reviewer.user,
            state: reviewer
                .merge_request_interaction
                .and_then(|interaction| interaction.review_state)
                .unwrap_or_default()
                .to_ascii_lowercase(),
        })
        .collect();
    let approvals = WireApprovals {
        approved_by: approvals
            .into_iter()
            .map(|user| WireApproval { user })
            .collect(),
    };
    let (discussion, threads) = discussion(notes, number)?;

    Ok(PullRequestOverview {
        summary: Summary {
            author: mr
                .author
                .as_ref()
                .map(WireUser::display)
                .unwrap_or_default(),
            base_ref: mr.target_branch,
            head_ref: mr.source_branch,
            additions: stats.additions,
            deletions: stats.deletions,
            changed_files: stats.file_count,
            updated_on: mr.updated_at,
            comments: discussion.len() as u32,
            checks,
            reviewers: wire::reviewers(&requested, &approvals),
            threads,
        },
        body: mr.description.unwrap_or_default(),
        discussion,
    })
}

fn request<T: DeserializeOwned>(
    url: &str,
    token: Option<&str>,
    query: &str,
    variables: &Value,
    path: &str,
) -> Result<T> {
    let mut response = transport::graphql(url, token, query, variables)?;
    let value = response.pointer_mut(path).filter(|value| !value.is_null())
        .context("GitLab overview is unavailable; check the project and authentication")?;
    serde_json::from_value(value.take())
        .context("failed to parse GitLab overview")
}

fn complete<T: DeserializeOwned>(
    url: &str,
    token: Option<&str>,
    mut page: Connection<T>,
    query: &str,
    mut variables: Value,
    path: &str,
) -> Result<Vec<T>> {
    let mut items = Vec::new();
    let mut cursors = HashSet::new();
    loop {
        items.extend(page.nodes);
        if !page.page_info.has_next_page {
            return Ok(items);
        }

        let cursor = page
            .page_info
            .end_cursor
            .filter(|cursor| !cursor.is_empty())
            .context("GitLab pagination is missing its next cursor")?;
        if !cursors.insert(cursor.clone()) {
            bail!("GitLab pagination did not advance");
        }
        variables["after"] = json!(cursor);
        page = request(url, token, query, &variables, path)?;
    }
}

fn local_id(id: &str) -> Result<&str> {
    id.strip_prefix("gid://gitlab/")
        .and_then(|id| id.split_once('/'))
        .map(|(_, id)| id)
        .filter(|id| !id.is_empty() && !id.contains('/'))
        .context("invalid GitLab GraphQL ID")
}

fn discussion(
    notes: Vec<Note>,
    number: u32,
) -> Result<(Vec<Comment>, Threads)> {
    let mut groups: Vec<Vec<Note>> = Vec::new();
    let mut indices = HashMap::new();
    for note in notes.into_iter().filter(|note| !note.system) {
        let index =
            *indices
                .entry(note.discussion.id.clone())
                .or_insert_with(|| {
                    groups.push(Vec::new());
                    groups.len() - 1
                });
        groups[index].push(note);
    }

    let mut comments = Vec::new();
    let mut threads = Threads {
        unresolved: 0,
        total: 0,
        is_truncated: false,
    };
    for notes in groups {
        let first = &notes[0];
        if first.position.is_some() {
            threads.total += 1;
            threads.unresolved += u32::from(!first.resolved);
            continue;
        }

        for note in notes {
            let id = local_id(&note.id)?
                .parse()
                .context("invalid GitLab note ID")?;
            let discussion = local_id(&note.discussion.id)?.to_owned();
            comments.push(Comment {
                id: Arc::from(
                    CommentRef::Published {
                        mr: number,
                        note: id,
                    }
                    .to_string(),
                ),
                reply_target: Some(Arc::from(
                    ReplyRef {
                        discussion,
                        note: id,
                    }
                    .to_string(),
                )),
                author: note
                    .author
                    .as_ref()
                    .map(WireUser::display)
                    .unwrap_or_default(),
                body: note.body,
                created_at: note.created_at,
                is_pending: false,
            });
        }
    }

    Ok((comments, threads))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(id: u32, discussion: &str, is_inline: bool) -> Note {
        serde_json::from_value(json!({
            "id": format!("gid://gitlab/Note/{id}"),
            "body": format!("Note {id}"),
            "createdAt": "2026-09-16T00:00:00Z",
            "system": false, "resolved": false,
            "position": is_inline.then(|| json!({"positionType":"text"})),
            "author": {"username":"alice", "name":"Alice"},
            "discussion": {"id":format!("gid://gitlab/Discussion/{discussion}")}
        }))
        .unwrap()
    }

    #[test]
    fn discussion_separates_threads_and_preserves_reply_targets() {
        let mut resolved = note(3, "resolved", true);
        resolved.resolved = true;
        let notes = vec![
            note(1, "general", false),
            note(2, "inline", true),
            resolved,
            note(4, "general", false),
            note(5, "inline", false),
        ];
        let (comments, threads) = discussion(notes, 42).unwrap();
        assert_eq!(threads.total, 2);
        assert_eq!(threads.unresolved, 1);
        assert!(!threads.is_truncated);
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0].id.as_ref(), "42:n1");
        assert_eq!(comments[1].reply_target.as_deref(), Some("general:4"));
        assert_eq!(comments[0].author, "Alice");
        assert_eq!(comments[0].body, "Note 1");
    }

    #[test]
    fn discussion_omits_system_notes_and_handles_missing_authors() {
        let mut system = note(1, "system", false);
        system.system = true;
        let mut deleted_author = note(2, "general", false);
        deleted_author.author = None;
        let (comments, threads) =
            discussion(vec![system, deleted_author], 42).unwrap();
        assert_eq!(threads.total, 0);
        assert_eq!(comments.len(), 1);
        assert!(comments[0].author.is_empty());
    }

    #[test]
    fn malformed_graphql_ids_are_rejected() {
        for id in [
            "123",
            "gid://github/Note/1",
            "gid://gitlab/Note/",
            "gid://gitlab/Note/a/b",
        ] {
            assert!(local_id(id).is_err());
        }
    }
}
