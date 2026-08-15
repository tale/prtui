use anyhow::{Context, Result, bail};
use tokio::process::Command;

const THREADS_QUERY: &str = r"
query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      number title state isDraft additions deletions changedFiles
      author { login }
      baseRefName headRefName body
      reviewThreads(first:100) {
        nodes {
          id isResolved isOutdated path line originalLine diffSide
          comments(first:50) { nodes { id author { login } body createdAt } }
        }
      }
      reviews(first:50) { nodes { author { login } state submittedAt } }
    }
  }
}
";

#[derive(Clone)]
pub struct Repo {
    pub host: Option<String>,
    pub owner: String,
    pub name: String,
}

impl Repo {
    /// Accepts `OWNER/REPO` or `HOST/OWNER/REPO`, matching `gh -R`.
    pub fn parse(slug: &str) -> Result<Self> {
        let parts: Vec<&str> =
            slug.trim().split('/').filter(|s| !s.is_empty()).collect();

        let (host, owner, name) = match parts.as_slice() {
            [owner, name] => (None, *owner, *name),
            [host, owner, name] => (Some(host.to_string()), *owner, *name),
            _ => bail!("expected [HOST/]OWNER/REPO, got {slug}"),
        };

        Ok(Self {
            host,
            owner: owner.to_string(),
            name: name.to_string(),
        })
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
pub async fn fetch_files(
    repo: &Repo,
    number: u32,
) -> Result<serde_json::Value> {
    let path = format!(
        "/repos/{}/{}/pulls/{number}/files?per_page=100",
        repo.owner, repo.name
    );

    let mut args = vec![
        "api".to_string(),
        path,
        "--paginate".into(),
        "--slurp".into(),
    ];
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

    let val: serde_json::Value = serde_json::from_slice(&raw)
        .context("failed to parse graphql response")?;

    if let Some(errors) = val.get("errors") {
        bail!("graphql error: {errors}");
    }

    Ok(val)
}

/// Download a comment attachment. Attachments on github.com itself are private
/// to the repository, so they carry the CLI's token; curl drops the header if
/// the download redirects to a storage host.
pub async fn fetch_asset(url: &str) -> Result<Vec<u8>> {
    let mut args = vec![
        "--silent".to_string(),
        "--show-error".into(),
        "--location".into(),
        "--max-time".into(),
        "20".into(),
        "--max-filesize".into(),
        "26214400".into(),
    ];

    if url.starts_with("https://github.com/")
        && let Some(token) = auth_token().await
    {
        args.push("--header".into());
        args.push(format!("Authorization: Bearer {token}"));
    }
    args.push(url.to_string());

    let out = Command::new("curl")
        .args(&args)
        .output()
        .await
        .context("failed to spawn curl")?;

    if !out.status.success() {
        bail!(
            "curl failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }

    Ok(out.stdout)
}

async fn auth_token() -> Option<String> {
    let raw = run(&["auth".to_string(), "token".into()]).await.ok()?;
    let token = String::from_utf8_lossy(&raw).trim().to_string();

    (!token.is_empty()).then_some(token)
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
