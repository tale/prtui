use anyhow::{bail, Context, Result};
use tokio::process::Command;

const THREADS_QUERY: &str = r#"
query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      number title state isDraft additions deletions changedFiles
      author { login }
      baseRefName headRefName body
      reviewThreads(first:100) {
        nodes {
          id isResolved isOutdated path line originalLine diffSide
          comments(first:50) { nodes { author { login } body createdAt } }
        }
      }
      reviews(first:50) { nodes { author { login } state submittedAt } }
    }
  }
}
"#;

#[derive(Clone)]
pub struct Repo {
    pub host: Option<String>,
    pub owner: String,
    pub name: String,
}

impl Repo {
    /// Accepts `OWNER/REPO` or `HOST/OWNER/REPO`, matching `gh -R`.
    pub fn parse(slug: &str) -> Result<Self> {
        let parts: Vec<&str> = slug.trim().split('/').filter(|s| !s.is_empty()).collect();

        let (host, owner, name) = match parts.as_slice() {
            [owner, name] => (None, *owner, *name),
            [host, owner, name] => (Some(host.to_string()), *owner, *name),
            _ => bail!("expected [HOST/]OWNER/REPO, got {slug}"),
        };

        Ok(Self { host, owner: owner.to_string(), name: name.to_string() })
    }

    fn host_args(&self) -> Vec<String> {
        match &self.host {
            Some(host) => vec!["--hostname".into(), host.clone()],
            None => Vec::new(),
        }
    }
}

async fn run(args: &[String]) -> Result<Vec<u8>> {
    let out = Command::new("gh")
        .args(args)
        .output()
        .await
        .context("failed to spawn gh; is it installed and on PATH?")?;

    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("gh {} failed: {}", args.join(" "), err.trim());
    }

    Ok(out.stdout)
}

/// Changed files with their unified-diff patches. Measured faster than the
/// `Accept: v3.diff` endpoint, and arrives pre-split per file.
pub async fn fetch_files(repo: &Repo, number: u32) -> Result<serde_json::Value> {
    let path = format!(
        "/repos/{}/{}/pulls/{number}/files?per_page=100",
        repo.owner, repo.name
    );

    let mut args = vec!["api".to_string(), path, "--paginate".into(), "--slurp".into()];
    args.extend(repo.host_args());

    let raw = run(&args).await?;

    serde_json::from_slice(&raw).context("failed to parse /files response")
}

/// PR metadata plus review threads in a single GraphQL round trip.
pub async fn fetch_meta(repo: &Repo, number: u32) -> Result<serde_json::Value> {
    let mut args = vec![
        "api".to_string(),
        "graphql".into(),
        "-F".into(),
        format!("owner={}", repo.owner),
        "-F".into(),
        format!("repo={}", repo.name),
        "-F".into(),
        format!("number={number}"),
        "-f".into(),
        format!("query={THREADS_QUERY}"),
    ];
    args.extend(repo.host_args());

    let raw = run(&args).await?;

    let val: serde_json::Value =
        serde_json::from_slice(&raw).context("failed to parse graphql response")?;

    if let Some(errors) = val.get("errors") {
        bail!("graphql error: {errors}");
    }

    Ok(val)
}

pub async fn current_repo() -> Result<Repo> {
    let raw = run(&[
        "repo".to_string(),
        "view".into(),
        "--json".into(),
        "nameWithOwner".into(),
        "--jq".into(),
        ".nameWithOwner".into(),
    ])
    .await?;

    Repo::parse(String::from_utf8_lossy(&raw).trim())
}
