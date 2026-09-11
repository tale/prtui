use anyhow::{Context, Result};
use prtui_core::{
    Anchor, LineKind, NewThread, Parent, Provider, ReviewEvent, Side,
};
use prtui_gitlab::GitLab;
use std::sync::Arc;

fn target() -> Result<(prtui_core::Repo, u32)> {
    let slug = std::env::var("PRTUI_GITLAB_TEST_REPO").context(
        "set PRTUI_GITLAB_TEST_REPO to an expendable GitLab test project",
    )?;
    let number = std::env::var("PRTUI_GITLAB_TEST_MR")?.parse()?;
    Ok((GitLab.parse_repo(&slug)?, number))
}

#[tokio::test]
#[ignore = "requires an authenticated GitLab test project"]
async fn read_live_merge_request() -> Result<()> {
    let (repo, number) = target()?;
    let files = GitLab.fetch_files(&repo, number).await?;
    let meta = GitLab.fetch_meta(&repo, number).await?;
    let overview = GitLab.fetch_overview(&repo, number).await?;
    let listing = GitLab.repository_pull_requests(repo.clone()).await?;
    assert!(
        listing
            .items
            .iter()
            .any(|item| item.target.number == number)
    );
    if std::env::var("GITLAB_HOST").as_deref()
        == Ok(prtui_gitlab::host_of(&repo))
    {
        let authored = GitLab.user_pull_requests().await?;
        assert!(
            authored
                .items
                .iter()
                .any(|item| item.target.number == number
                    && item.target.repo.namespace == repo.namespace
                    && item.target.repo.name == repo.name)
        );
        println!("authored listing: {} merge requests", authored.items.len());
    }
    assert_eq!(overview.summary.changed_files as usize, files.len());
    assert_eq!(overview.body, meta.pr.body);
    println!(
        "MR !{number}: {} files, {} threads, {} conversation notes",
        files.len(),
        meta.threads.len(),
        meta.discussion.len()
    );

    for file in files.iter().filter(|file| file.status != "removed").take(8) {
        let blob = GitLab
            .fetch_blob(&repo, &file.path, &meta.pr.head_oid)
            .await?;
        println!("blob {}: {} bytes", file.path, blob.len());
    }
    Ok(())
}

#[tokio::test]
#[ignore = "creates and publishes comments; use only an expendable merge request"]
async fn exercise_live_review() -> Result<()> {
    let (repo, number) = target()?;
    anyhow::ensure!(
        std::env::var("PRTUI_GITLAB_TEST_WRITE").as_deref() == Ok("yes"),
        "set PRTUI_GITLAB_TEST_WRITE=yes to authorize test comments"
    );
    let files = GitLab.fetch_files(&repo, number).await?;
    let before = GitLab.fetch_meta(&repo, number).await?;
    anyhow::ensure!(
        before.pending_review.is_none(),
        "use an MR without existing drafts"
    );
    let prefix = format!("prtui-live-{}", std::process::id());
    let mut cases = Vec::new();
    let initial_seed = std::env::var("PRTUI_GITLAB_TEST_SEED")
        .map_or(Ok(0x5eed_u64), |seed| seed.parse())?;
    let mut seed = initial_seed;
    for file in files
        .iter()
        .filter(|file| !file.path.starts_with("paging/"))
    {
        cases.push((Arc::clone(&file.path), None));
        let lines: Vec<_> = file
            .lines
            .iter()
            .filter(|line| line.kind != LineKind::Hunk)
            .collect();
        for line in &lines {
            let (number, side) = line.new_line.map_or_else(
                || (line.old_line.unwrap(), Side::Left),
                |line| (line, Side::Right),
            );
            cases.push((
                Arc::clone(&file.path),
                Some(Anchor::spanning(number, number, side)),
            ));
        }
        for pair in file
            .lines
            .windows(2)
            .filter(|pair| pair.iter().all(|line| line.kind != LineKind::Hunk))
        {
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            if seed % 3 != 0 {
                continue;
            }
            let start = &pair[0];
            let end = &pair[1];
            let start_side = if start.new_line.is_none()
                || (start.old_line.is_some() && seed & 8 != 0)
            {
                Side::Left
            } else {
                Side::Right
            };
            let side = if end.new_line.is_none()
                || (end.old_line.is_some() && seed & 32 != 0)
            {
                Side::Left
            } else {
                Side::Right
            };
            let start_line = if start_side == Side::Left {
                start.old_line.unwrap()
            } else {
                start.new_line.unwrap()
            };
            let end_line = if side == Side::Left {
                end.old_line.unwrap()
            } else {
                end.new_line.unwrap()
            };
            cases.push((
                Arc::clone(&file.path),
                Some(Anchor {
                    start_line,
                    start_side,
                    end_line,
                    side,
                }),
            ));
        }
    }
    for file in files.iter().filter(|file| {
        file.path.as_ref() == "cases/context.txt"
            || file.path.as_ref() == "cases/sp ace%#é.txt"
    }) {
        for (start, end) in
            [(1, 1), (25, 25), (60, 60), (1, 2), (20, 23), (57, 59)]
        {
            cases.push((
                Arc::clone(&file.path),
                Some(Anchor::spanning(start, end, Side::Right)),
            ));
        }
    }
    anyhow::ensure!(!cases.is_empty(), "fixture needs changed files");
    let originals = cases.clone();
    while cases.len() < 105 {
        cases.extend(originals.clone());
    }
    cases.truncate(240);
    println!(
        "seed={initial_seed:#x}, {} comment cases across {} files",
        cases.len(),
        files.len()
    );
    let mut created = Vec::new();
    let mut failures = Vec::new();
    for (index, (path, anchor)) in cases.iter().enumerate() {
        let body = format!(
            "{prefix}:{index} — Unicode café 日本語\n\n`code`\n\nline two"
        );
        match GitLab
            .add_thread(
                &repo,
                NewThread {
                    parent: Parent::PullRequest(Arc::clone(&before.pr.id)),
                    path: Arc::clone(path),
                    body: body.clone(),
                    anchor: *anchor,
                },
            )
            .await
        {
            Ok(added) => created.push((index, added.comment, body)),
            Err(error) => {
                println!("FAIL create {index} {path} {anchor:?}: {error:#}");
                failures.push(index);
            }
        }
        if (index + 1) % 10 == 0 {
            println!("created {}/{}", index + 1, cases.len());
        }
    }
    let drafts = GitLab.fetch_meta(&repo, number).await?;
    for (index, id, _) in &created {
        let thread = drafts
            .threads
            .iter()
            .find(|thread| {
                thread.comments.iter().any(|comment| comment.id == *id)
            })
            .context("saved draft lost its diff position")?;
        let (path, anchor) = &cases[*index];
        assert_eq!(&thread.path, path, "case {index}");
        if let Some(anchor) = anchor {
            assert_eq!(thread.line, Some(anchor.end_line), "case {index}");
            assert_eq!(thread.side, anchor.side, "case {index}");
            if anchor.is_multiline() {
                assert_eq!(
                    thread.start_line,
                    Some(anchor.start_line),
                    "case {index}"
                );
                assert_eq!(
                    thread.start_side,
                    Some(anchor.start_side),
                    "case {index}"
                );
            }
        } else {
            assert!(thread.is_file_level, "case {index}");
        }
    }
    let pending: Vec<_> = drafts
        .threads
        .iter()
        .flat_map(|thread| &thread.comments)
        .chain(&drafts.discussion)
        .filter(|comment| comment.body.starts_with(&prefix))
        .collect();
    assert_eq!(pending.len(), created.len(), "draft pagination lost notes");
    for (_, id, body) in &created {
        let comment = pending
            .iter()
            .find(|comment| comment.id == *id)
            .context("missing saved draft")?;
        assert_eq!(&comment.body, body);
        assert!(comment.is_pending);
    }
    println!("verified {} saved drafts", pending.len());
    for (index, id, _) in created.iter().step_by(17) {
        GitLab
            .update_comment(
                &repo,
                Arc::clone(id),
                format!("{prefix}:{index} edited\n\n保持位置"),
            )
            .await?;
    }
    let edited = GitLab.fetch_meta(&repo, number).await?;
    for (index, id, _) in created.iter().step_by(17) {
        let thread = edited
            .threads
            .iter()
            .find(|thread| {
                thread.comments.iter().any(|comment| comment.id == *id)
            })
            .context("draft edit detached its position")?;
        assert_eq!(thread.path, cases[*index].0);
        assert!(thread.comments[0].body.contains("保持位置"));
    }
    println!("verified draft edits preserve positions");
    let (_, deleted_id, _) = created.pop().context("no draft to delete")?;
    GitLab
        .delete_comment(&repo, Arc::clone(&deleted_id))
        .await?;
    let after_delete = GitLab.fetch_meta(&repo, number).await?;
    assert!(
        !after_delete
            .threads
            .iter()
            .flat_map(|thread| &thread.comments)
            .chain(&after_delete.discussion)
            .any(|comment| comment.id == deleted_id)
    );
    println!("verified draft deletion");
    GitLab
        .submit_review(
            &repo,
            Parent::PullRequest(Arc::clone(&before.pr.id)),
            ReviewEvent::Comment,
            format!("{prefix}: review lifecycle test"),
        )
        .await?;
    let published = GitLab.fetch_meta(&repo, number).await?;
    let threads: Vec<_> = published
        .threads
        .iter()
        .filter(|thread| {
            thread
                .comments
                .iter()
                .any(|comment| comment.body.starts_with(&prefix))
        })
        .collect();
    assert_eq!(
        threads.len(),
        created.len(),
        "publication lost diff threads"
    );
    assert!(published.pending_review.is_none());
    println!("verified {} published threads", threads.len());
    for thread in threads.iter().take(3) {
        let comment = &thread.comments[0];
        GitLab
            .reply(
                &repo,
                number,
                Arc::clone(
                    comment
                        .reply_target
                        .as_ref()
                        .context("missing reply target")?,
                ),
                format!("{prefix}: reply"),
            )
            .await?;
        if thread.can_resolve {
            GitLab
                .set_resolved(&repo, Arc::clone(&thread.id), true)
                .await?;
            let resolved = GitLab.fetch_meta(&repo, number).await?;
            assert!(
                resolved
                    .threads
                    .iter()
                    .find(|saved| saved.id == thread.id)
                    .context("missing resolved thread")?
                    .is_resolved
            );
            GitLab
                .set_resolved(&repo, Arc::clone(&thread.id), false)
                .await?;
            let unresolved = GitLab.fetch_meta(&repo, number).await?;
            assert!(
                !unresolved
                    .threads
                    .iter()
                    .find(|saved| saved.id == thread.id)
                    .context("missing unresolved thread")?
                    .is_resolved
            );
        }
        GitLab
            .update_comment(
                &repo,
                Arc::clone(&comment.id),
                format!("{prefix}: published edit"),
            )
            .await?;
    }
    let meta = GitLab.fetch_meta(&repo, number).await?;
    assert!(meta.threads.iter().any(|thread| {
        thread
            .comments
            .iter()
            .any(|comment| comment.body == format!("{prefix}: reply"))
    }));
    for thread in threads.iter().rev().take(3) {
        GitLab
            .delete_comment(&repo, Arc::clone(&thread.comments[0].id))
            .await?;
    }
    println!("reply, resolve, unresolve, published edit and delete completed");
    anyhow::ensure!(
        failures.is_empty(),
        "{} of {} generated cases failed: {failures:?}",
        failures.len(),
        cases.len()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "compares saved positions against the live GitLab API"]
async fn compare_live_positions() -> Result<()> {
    let (repo, number) = target()?;
    let project = std::env::var("PRTUI_GITLAB_TEST_PROJECT_ID")?;
    let output = tokio::process::Command::new("glab")
        .args([
            "api",
            "--hostname",
            prtui_gitlab::host_of(&repo),
            &format!("projects/{project}/merge_requests/{number}/draft_notes"),
            "--paginate",
        ])
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "glab draft lookup failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let drafts = cli_pages(&output.stdout)?;
    let mut raw: Vec<_> = drafts
        .into_iter()
        .map(|draft| {
            (
                format!("{number}:d{}", draft["id"]),
                draft["position"].clone(),
            )
        })
        .collect();
    let output = tokio::process::Command::new("glab")
        .args([
            "api",
            "--hostname",
            prtui_gitlab::host_of(&repo),
            &format!("projects/{project}/merge_requests/{number}/discussions"),
            "--paginate",
        ])
        .output()
        .await?;
    anyhow::ensure!(output.status.success(), "glab discussion lookup failed");
    let discussions = cli_pages(&output.stdout)?;
    for discussion in discussions {
        if let Some(note) = discussion["notes"]
            .as_array()
            .and_then(|notes| notes.first())
        {
            raw.push((
                format!("{number}:n{}", note["id"]),
                note["position"].clone(),
            ));
        }
    }
    let meta = GitLab.fetch_meta(&repo, number).await?;
    let mut checked = 0;
    for (id, position) in raw {
        if position["line_range"].get("start").is_none() {
            continue;
        }
        let thread = meta
            .threads
            .iter()
            .find(|thread| {
                thread
                    .comments
                    .iter()
                    .any(|comment| comment.id.as_ref() == id)
            })
            .context("missing saved comment")?;
        for (key, line, side) in [
            ("start", thread.start_line, thread.start_side),
            ("end", thread.line, Some(thread.side)),
        ] {
            let point = &position["line_range"][key];
            let (expected_side, field) = if point["type"] == "old" {
                (Side::Left, "old_line")
            } else {
                (Side::Right, "new_line")
            };
            assert_eq!(
                side,
                Some(expected_side),
                "comment {id}, raw {key} {point}"
            );
            assert_eq!(
                line,
                point[field].as_u64().map(|line| line as u32),
                "comment {id}, {key}"
            );
        }
        checked += 1;
    }
    anyhow::ensure!(
        checked > 0,
        "fixture has no multiline positions to compare"
    );
    println!("compared {checked} multiline positions with raw API responses");
    Ok(())
}

fn cli_pages(bytes: &[u8]) -> Result<Vec<serde_json::Value>> {
    let pages = serde_json::Deserializer::from_slice(bytes)
        .into_iter::<Vec<serde_json::Value>>()
        .collect::<Result<Vec<_>, _>>()?;
    Ok(pages.into_iter().flatten().collect())
}

#[tokio::test]
#[ignore = "publishes a summary and attempts approval on a disposable merge request"]
async fn exercise_live_review_edges() -> Result<()> {
    let (repo, number) = target()?;
    anyhow::ensure!(
        std::env::var("PRTUI_GITLAB_TEST_WRITE").as_deref() == Ok("yes"),
        "set PRTUI_GITLAB_TEST_WRITE=yes"
    );
    let files = GitLab.fetch_files(&repo, number).await?;
    let before = GitLab.fetch_meta(&repo, number).await?;
    anyhow::ensure!(
        before.pending_review.is_none(),
        "use an MR without existing drafts"
    );
    let file = files
        .iter()
        .find(|file| !file.lines.is_empty())
        .context("no text diff")?;
    let error = GitLab
        .add_thread(
            &repo,
            NewThread {
                parent: Parent::PullRequest(Arc::clone(&before.pr.id)),
                path: Arc::clone(&file.path),
                body: "must not save line zero".to_owned(),
                anchor: Some(Anchor::spanning(0, 0, Side::Right)),
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("positive"));
    let body = format!(
        "prtui-live-summary-{}: no pending drafts — 日本語",
        std::process::id()
    );
    GitLab
        .submit_review(
            &repo,
            Parent::PullRequest(Arc::clone(&before.pr.id)),
            ReviewEvent::Comment,
            body.clone(),
        )
        .await?;
    let after = GitLab.fetch_meta(&repo, number).await?;
    let comment = after
        .discussion
        .iter()
        .find(|comment| comment.body == body)
        .context("summary-only review was not published")?;
    GitLab
        .delete_comment(&repo, Arc::clone(&comment.id))
        .await?;
    println!("line-zero rejection and summary-only review passed");
    match GitLab
        .submit_review(
            &repo,
            Parent::PullRequest(Arc::clone(&before.pr.id)),
            ReviewEvent::Approve,
            String::new(),
        )
        .await
    {
        Ok(()) => println!("approval accepted"),
        Err(error) => {
            let message = error.to_string();
            anyhow::ensure!(
                message.contains("approval failed: HTTP 401")
                    || message.contains("approval failed: HTTP 403")
                    || message.contains("approval failed: HTTP 400"),
                "unexpected approval error: {error:#}"
            );
            println!("approval denied by GitLab: {message}");
        }
    }
    Ok(())
}

#[tokio::test]
#[ignore = "changes review verdicts on a disposable MR with the current user assigned as reviewer"]
async fn exercise_live_review_states() -> Result<()> {
    let (repo, number) = target()?;
    anyhow::ensure!(
        std::env::var("PRTUI_GITLAB_TEST_WRITE").as_deref() == Ok("yes"),
        "set PRTUI_GITLAB_TEST_WRITE=yes"
    );
    let output = tokio::process::Command::new("glab")
        .args(["api", "--hostname", prtui_gitlab::host_of(&repo), "user"])
        .output()
        .await?;
    anyhow::ensure!(output.status.success(), "could not read current user");
    let viewer: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    let name = viewer["name"]
        .as_str()
        .or_else(|| viewer["username"].as_str())
        .context("current user has no name")?;
    let meta = GitLab.fetch_meta(&repo, number).await?;
    anyhow::ensure!(
        meta.pending_review.is_none(),
        "use an MR without existing drafts"
    );
    let parent = Parent::PullRequest(Arc::clone(&meta.pr.id));
    let prefix = format!("prtui-review-states-{}", std::process::id());

    for (event, verdict) in [
        (
            ReviewEvent::RequestChanges,
            prtui_core::Verdict::ChangesRequested,
        ),
        (ReviewEvent::Approve, prtui_core::Verdict::Approved),
    ] {
        GitLab
            .submit_review(
                &repo,
                parent.clone(),
                event,
                format!("{prefix}: {}", event.label()),
            )
            .await?;
        let summary = GitLab.fetch_summary(&repo, number).await?;
        assert_eq!(
            summary
                .reviewers
                .iter()
                .find(|reviewer| reviewer.name == name)
                .context("viewer missing from summary")?
                .verdict,
            verdict
        );
        println!("verified {} without drafts", event.label());
    }

    let files = GitLab.fetch_files(&repo, number).await?;
    let file = files
        .iter()
        .find(|file| !file.lines.is_empty())
        .context("no text diff")?;
    let draft = GitLab
        .add_thread(
            &repo,
            NewThread {
                parent: parent.clone(),
                path: Arc::clone(&file.path),
                body: format!("{prefix}: inline feedback"),
                anchor: None,
            },
        )
        .await?;
    GitLab
        .submit_review(
            &repo,
            Parent::Review(draft.review),
            ReviewEvent::RequestChanges,
            format!("{prefix}: requested changes with a draft"),
        )
        .await?;
    let published = GitLab.fetch_meta(&repo, number).await?;
    assert!(published.pending_review.is_none());
    assert!(
        published
            .threads
            .iter()
            .flat_map(|thread| &thread.comments)
            .any(|comment| comment.body
                == format!("{prefix}: inline feedback")
                && !comment.is_pending)
    );
    let summary = GitLab.fetch_summary(&repo, number).await?;
    assert_eq!(
        summary
            .reviewers
            .iter()
            .find(|reviewer| reviewer.name == name)
            .unwrap()
            .verdict,
        prtui_core::Verdict::ChangesRequested
    );
    println!(
        "verified requested changes publishes drafts and replaces approval"
    );

    GitLab
        .submit_review(
            &repo,
            parent,
            ReviewEvent::Comment,
            format!("{prefix}: reviewed after changes"),
        )
        .await?;
    let summary = GitLab.fetch_summary(&repo, number).await?;
    assert_eq!(
        summary
            .reviewers
            .iter()
            .find(|reviewer| reviewer.name == name)
            .unwrap()
            .verdict,
        prtui_core::Verdict::Commented
    );
    println!("verified reviewed clears the requested-changes verdict");
    Ok(())
}

#[tokio::test]
#[ignore = "temporarily removes the author from reviewers on a disposable self-authored MR"]
async fn reports_unrecorded_live_review() -> Result<()> {
    let (repo, number) = target()?;
    anyhow::ensure!(
        std::env::var("PRTUI_GITLAB_TEST_WRITE").as_deref() == Ok("yes"),
        "set PRTUI_GITLAB_TEST_WRITE=yes"
    );
    let project = std::env::var("PRTUI_GITLAB_TEST_PROJECT_ID")?;
    let host = prtui_gitlab::host_of(&repo);
    let endpoint = format!("projects/{project}/merge_requests/{number}");
    let meta = GitLab.fetch_meta(&repo, number).await?;
    anyhow::ensure!(
        meta.pending_review.is_none(),
        "use an MR without existing drafts"
    );
    let output = tokio::process::Command::new("glab")
        .args(["api", "--hostname", host, "user"])
        .output()
        .await?;
    anyhow::ensure!(output.status.success(), "cannot read current user");
    let viewer: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    anyhow::ensure!(
        viewer["name"] == meta.pr.author,
        "this test requires a self-authored MR"
    );
    let output = tokio::process::Command::new("glab")
        .args(["api", "--hostname", host, &format!("{endpoint}/reviewers")])
        .output()
        .await?;
    anyhow::ensure!(output.status.success(), "cannot read original reviewers");
    let original: Vec<serde_json::Value> =
        serde_json::from_slice(&output.stdout)?;
    let original_ids: Vec<_> = original
        .iter()
        .map(|reviewer| reviewer["user"]["id"].clone())
        .collect();
    let remaining: Vec<_> = original_ids
        .iter()
        .filter(|id| **id != viewer["id"])
        .cloned()
        .collect();
    set_live_reviewers(host, &endpoint, &remaining).await?;

    let result = GitLab.submit_review(&repo, Parent::PullRequest(Arc::clone(&meta.pr.id)), ReviewEvent::RequestChanges, "prtui live test: unassigned author cannot record a reviewer verdict".to_owned()).await;
    set_live_reviewers(host, &endpoint, &original_ids).await?;
    GitLab
        .submit_review(
            &repo,
            Parent::PullRequest(meta.pr.id),
            ReviewEvent::Comment,
            "prtui live test: restored reviewers after missing-verdict check"
                .to_owned(),
        )
        .await?;

    let error = result.expect_err(
        "an unassigned MR author should not acquire a reviewer state",
    );
    assert!(error.to_string().contains("comments were published, but GitLab did not record requested changes"), "{error:#}");
    println!(
        "verified an HTTP-success response without a recorded verdict is reported as a partial failure"
    );
    Ok(())
}

async fn set_live_reviewers(
    host: &str,
    endpoint: &str,
    ids: &[serde_json::Value],
) -> Result<()> {
    let field = format!("reviewer_ids={}", serde_json::to_string(ids)?);
    let output = tokio::process::Command::new("glab")
        .args([
            "api",
            "--hostname",
            host,
            endpoint,
            "--method",
            "PUT",
            "--field",
            &field,
        ])
        .output()
        .await?;
    anyhow::ensure!(
        output.status.success(),
        "cannot update fixture reviewers: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}
