//! How a project can be addressed on one host.

use anyhow::{Context, Result, bail};
use prtui_core::Repo;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use crate::url::escape_segment;
use crate::{host_of, transport};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProjectRef {
    Path(String),
    Id(u64),
}

impl ProjectRef {
    pub const fn rewrites_separators(&self) -> bool {
        matches!(self, Self::Id(_))
    }
}

impl std::fmt::Display for ProjectRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Path(path) => f.write_str(path),
            Self::Id(id) => write!(f, "{id}"),
        }
    }
}

#[derive(Debug, Deserialize)]
struct WireProject {
    id: u64,
    path_with_namespace: String,
}

fn cache() -> &'static Mutex<HashMap<String, ProjectRef>> {
    static REFS: OnceLock<Mutex<HashMap<String, ProjectRef>>> = OnceLock::new();

    REFS.get_or_init(Mutex::default)
}

fn full_path(repo: &Repo) -> String {
    format!("{}/{}", repo.namespace, repo.name)
}

pub async fn reference(repo: &Repo) -> Result<ProjectRef> {
    let key = repo.slug();

    if let Ok(cache) = cache().lock()
        && let Some(hit) = cache.get(&key)
    {
        return Ok(hit.clone());
    }

    let resolved = probe(repo).await?;
    if let Ok(mut cache) = cache().lock() {
        cache.insert(key, resolved.clone());
    }

    Ok(resolved)
}

async fn probe(repo: &Repo) -> Result<ProjectRef> {
    let encoded = escape_segment(&full_path(repo));
    let host = host_of(repo);
    let url = format!("https://{host}/api/v4/projects/{encoded}");
    let token = transport::token(host).await;

    let status = tokio::task::spawn_blocking(move || {
        let response = transport::get(&url, token.as_deref())?;

        anyhow::Ok(response.status())
    })
    .await
    .context("request task failed")??;

    if status.is_success() {
        return Ok(ProjectRef::Path(encoded));
    }

    if !status.is_redirection() {
        bail!(
            "cannot reach project {} on {host}: HTTP {status}",
            full_path(repo)
        );
    }

    Ok(ProjectRef::Id(search(repo).await?))
}

async fn search(repo: &Repo) -> Result<u64> {
    let host = host_of(repo);
    let wanted = full_path(repo);
    let found: Vec<WireProject> = crate::read_all_url(
        host,
        format!(
            "https://{host}/api/v4/projects?search={}&simple=true",
            escape_segment(&repo.name)
        ),
        "project lookup",
    )
    .await?;

    found
        .into_iter()
        .find(|project| {
            project.path_with_namespace.eq_ignore_ascii_case(&wanted)
        })
        .map(|project| project.id)
        .with_context(|| {
            format!(
                "project {wanted} not found on {host}. This host rewrites the \
                 percent-encoded `/` GitLab uses to name a project, so the \
                 project had to be looked up by name and no match was visible \
                 to this token."
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_identifier_renders_as_the_host_expects_it() {
        let path = ProjectRef::Path("group%2Fproject".to_owned());
        assert_eq!(path.to_string(), "group%2Fproject");
        assert!(!path.rewrites_separators());

        let id = ProjectRef::Id(7);
        assert_eq!(id.to_string(), "7");
        assert!(id.rewrites_separators());
    }
}
