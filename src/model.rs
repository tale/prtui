use anyhow::{Context, Result};
use serde::Deserialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Context,
    Added,
    Removed,
    Hunk,
}

#[derive(Debug, Clone)]
pub struct DiffLine {
    pub kind: LineKind,
    pub text: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
}

#[derive(Debug, Clone)]
pub struct ChangedFile {
    pub path: String,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub lines: Vec<DiffLine>,
}

#[derive(Debug, Deserialize)]
struct RawFile {
    filename: String,
    status: String,
    additions: u32,
    deletions: u32,
    patch: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Comment {
    pub author: String,
    pub body: String,
}

#[derive(Debug, Clone)]
pub struct ReviewThread {
    pub path: String,
    pub line: Option<u32>,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub comments: Vec<Comment>,
}

#[derive(Debug, Clone, Default)]
pub struct PullRequest {
    pub number: u32,
    pub title: String,
    pub state: String,
    pub is_draft: bool,
    pub author: String,
    pub base_ref: String,
    pub head_ref: String,
    pub body: String,
    pub threads: Vec<ReviewThread>,
}

/// `@@ -old,count +new,count @@` — captures the two start line numbers.
fn parse_hunk_header(header: &str) -> Option<(u32, u32)> {
    let inner = header.strip_prefix("@@ ")?.split(" @@").next()?;
    let (old, new) = inner.split_once(' ')?;

    let start = |s: &str| -> Option<u32> {
        s.get(1..)?.split(',').next()?.parse().ok()
    };

    Some((start(old)?, start(new)?))
}

fn parse_patch(patch: &str) -> Vec<DiffLine> {
    let mut lines = Vec::new();
    let mut old_line = 0;
    let mut new_line = 0;

    for raw in patch.lines() {
        if raw.starts_with("@@") {
            let Some((old, new)) = parse_hunk_header(raw) else { continue };

            old_line = old;
            new_line = new;
            lines.push(DiffLine {
                kind: LineKind::Hunk,
                text: raw.to_string(),
                old_line: None,
                new_line: None,
            });
            continue;
        }

        let (kind, text) = match raw.as_bytes().first() {
            Some(b'+') => (LineKind::Added, &raw[1..]),
            Some(b'-') => (LineKind::Removed, &raw[1..]),
            Some(b' ') => (LineKind::Context, &raw[1..]),
            _ => continue,
        };

        let (old, new) = match kind {
            LineKind::Added => (None, Some(new_line)),
            LineKind::Removed => (Some(old_line), None),
            _ => (Some(old_line), Some(new_line)),
        };

        if kind != LineKind::Added {
            old_line += 1;
        }
        if kind != LineKind::Removed {
            new_line += 1;
        }

        lines.push(DiffLine { kind, text: text.to_string(), old_line: old, new_line: new });
    }

    lines
}

/// `gh api --paginate --slurp` returns an array of pages, each an array of files.
pub fn parse_files(val: &serde_json::Value) -> Result<Vec<ChangedFile>> {
    let pages = val.as_array().context("expected array of pages from /files")?;

    let mut files = Vec::new();
    for page in pages {
        let raws: Vec<RawFile> =
            serde_json::from_value(page.clone()).context("unexpected /files page shape")?;

        for raw in raws {
            let lines = raw.patch.as_deref().map(parse_patch).unwrap_or_default();

            files.push(ChangedFile {
                path: raw.filename,
                status: raw.status,
                additions: raw.additions,
                deletions: raw.deletions,
                lines,
            });
        }
    }

    Ok(files)
}

fn text_at(val: &serde_json::Value, ptr: &str) -> String {
    val.pointer(ptr).and_then(|v| v.as_str()).unwrap_or_default().to_string()
}

pub fn parse_meta(val: &serde_json::Value) -> Result<PullRequest> {
    let pr = val
        .pointer("/data/repository/pullRequest")
        .context("PR not found in graphql response")?;

    let threads = pr
        .pointer("/reviewThreads/nodes")
        .and_then(|v| v.as_array())
        .map(|nodes| {
            nodes
                .iter()
                .map(|t| ReviewThread {
                    path: text_at(t, "/path"),
                    line: t
                        .get("line")
                        .and_then(|v| v.as_u64())
                        .or_else(|| t.get("originalLine").and_then(|v| v.as_u64()))
                        .map(|v| v as u32),
                    is_resolved: t.get("isResolved").and_then(|v| v.as_bool()).unwrap_or(false),
                    is_outdated: t.get("isOutdated").and_then(|v| v.as_bool()).unwrap_or(false),
                    comments: t
                        .pointer("/comments/nodes")
                        .and_then(|v| v.as_array())
                        .map(|cs| {
                            cs.iter()
                                .map(|c| Comment {
                                    author: text_at(c, "/author/login"),
                                    body: text_at(c, "/body"),
                                })
                                .collect()
                        })
                        .unwrap_or_default(),
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(PullRequest {
        number: pr.get("number").and_then(|v| v.as_u64()).unwrap_or(0) as u32,
        title: text_at(pr, "/title"),
        state: text_at(pr, "/state"),
        is_draft: pr.get("isDraft").and_then(|v| v.as_bool()).unwrap_or(false),
        author: text_at(pr, "/author/login"),
        base_ref: text_at(pr, "/baseRefName"),
        head_ref: text_at(pr, "/headRefName"),
        body: text_at(pr, "/body"),
        threads,
    })
}
