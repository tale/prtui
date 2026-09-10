use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use prtui_github::GitHub;
use tokio::process::Command;

use crate::{Args, ProviderChoice};

impl ProviderChoice {
    const ALL: &[Self] = &[Self::Github];

    const fn name(self) -> &'static str {
        match self {
            Self::Github => "github",
        }
    }

    const fn recognizes_host(self, host: &str) -> bool {
        match self {
            Self::Github => GitHub.recognizes_host(host),
        }
    }

    async fn probe_host(self, host: &str) -> bool {
        match self {
            Self::Github => GitHub.probe_host(host, PROBE_TIMEOUT).await,
        }
    }
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);

pub async fn resolve(args: &Args) -> Result<(ProviderChoice, Option<String>)> {
    let slug = match &args.repo {
        Some(slug) => Some(slug.clone()),
        None => current_remote(&std::env::current_dir()?).await?,
    };
    let default_host = std::env::var("GH_HOST")
        .ok()
        .filter(|host| !host.is_empty());
    let host = match slug.as_deref() {
        Some(slug) => slug_host(slug).unwrap_or("github.com"),
        None => default_host.as_deref().unwrap_or("github.com"),
    };
    let provider =
        select(host, args.provider, config_path().as_deref()).await?;

    Ok((provider, slug))
}

async fn select(
    host: &str,
    explicit: Option<ProviderChoice>,
    config: Option<&Path>,
) -> Result<ProviderChoice> {
    if let Some(provider) = explicit {
        return Ok(provider);
    }

    let host = host.to_ascii_lowercase();
    let mut hosts = read_hosts(config)?;
    if let Some(name) = hosts.get(&host) {
        return ProviderChoice::ALL.iter().copied().find(|provider| provider.name() == name)
            .with_context(|| format!("unsupported provider {name:?} saved for {host}; edit hosts.json or pass --provider"));
    }

    let detected = match ProviderChoice::ALL
        .iter()
        .copied()
        .find(|provider| provider.recognizes_host(&host))
    {
        Some(provider) => Some(provider),
        None => probe(&host).await?,
    };
    if let Some(provider) = detected {
        hosts.insert(host, provider.name().into());
        if let Some(path) = config
            && let Err(error) = write_hosts(path, &hosts)
        {
            eprintln!("could not save detected provider: {error:#}");
        }
        return Ok(provider);
    }

    Ok(ProviderChoice::Github)
}

async fn probe(host: &str) -> Result<Option<ProviderChoice>> {
    let mut probes = tokio::task::JoinSet::new();
    for &provider in ProviderChoice::ALL {
        let host = host.to_owned();
        probes.spawn(async move {
            let identified =
                tokio::time::timeout(PROBE_TIMEOUT, provider.probe_host(&host))
                    .await
                    .unwrap_or(false);
            identified.then_some(provider)
        });
    }
    let mut detected = None;
    while let Some(result) = probes.join_next().await {
        let Ok(Some(provider)) = result else {
            continue;
        };
        if detected.is_some() {
            bail!(
                "multiple providers recognized {host}; pass --provider to choose one"
            );
        }
        detected = Some(provider);
    }
    Ok(detected)
}

fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .map(|home| PathBuf::from(home).join(".config"))
        })?;
    Some(base.join("prtui/hosts.json"))
}

fn read_hosts(path: Option<&Path>) -> Result<BTreeMap<String, String>> {
    let Some(path) = path else {
        return Ok(BTreeMap::new());
    };
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(BTreeMap::new());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("reading {}", path.display()));
        }
    };
    serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing {}", path.display()))
}

fn write_hosts(path: &Path, hosts: &BTreeMap<String, String>) -> Result<()> {
    let parent = path
        .parent()
        .context("host configuration has no parent directory")?;
    std::fs::create_dir_all(parent)?;
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    let result = (|| {
        file.write_all(serde_json::to_string_pretty(hosts)?.as_bytes())?;
        std::fs::rename(&temporary, path)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

fn slug_host(slug: &str) -> Option<&str> {
    let (first, rest) = slug.trim().split_once('/')?;
    rest.contains('/').then_some(first)
}

async fn current_remote(directory: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .current_dir(directory)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .await
        .context("failed to spawn git")?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != "true"
    {
        return Ok(None);
    }

    let output = Command::new("git")
        .current_dir(directory)
        .arg("remote")
        .output()
        .await?;
    if !output.status.success() {
        bail!(
            "could not list Git remotes: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let names = String::from_utf8(output.stdout)?;
    let mut names: Vec<_> = names.lines().collect();
    names.sort_by_key(|name| (*name != "origin", *name));
    for name in names {
        let output = Command::new("git")
            .current_dir(directory)
            .args(["remote", "get-url", name])
            .output()
            .await?;
        if !output.status.success() {
            continue;
        }
        let url = String::from_utf8_lossy(&output.stdout);
        let url = url.trim();
        if let Some(mut slug) = remote_slug(url) {
            if url.starts_with("ssh://") || !url.contains("://") {
                slug = resolve_ssh_alias(&slug).await;
            }
            return Ok(Some(slug));
        }
    }

    bail!("no supported Git remote found; pass -R [HOST/]OWNER/REPO")
}

async fn resolve_ssh_alias(slug: &str) -> String {
    let Some((host, path)) = slug.split_once('/') else {
        return slug.to_owned();
    };
    if host.starts_with('-') {
        return slug.to_owned();
    }
    let mut command = Command::new("ssh");
    command.args(["-G", host]).kill_on_drop(true);
    let Ok(Ok(output)) =
        tokio::time::timeout(PROBE_TIMEOUT, command.output()).await
    else {
        return slug.to_owned();
    };
    if !output.status.success() {
        return slug.to_owned();
    }
    let config = String::from_utf8_lossy(&output.stdout);
    let hostname = config
        .lines()
        .find_map(|line| line.strip_prefix("hostname "));
    hostname
        .and_then(|host| remote_slug(&format!("https://{host}/{path}")))
        .unwrap_or_else(|| slug.to_owned())
}

fn remote_slug(url: &str) -> Option<String> {
    let (host, path) = if let Some((scheme, rest)) = url.split_once("://") {
        if !matches!(scheme, "https" | "http" | "ssh" | "git") {
            return None;
        }
        let (authority, path) = rest.split_once('/')?;
        let host = authority.rsplit('@').next()?;
        let host = if scheme == "ssh" {
            host.split(':').next()?
        } else {
            host
        };
        (host, path)
    } else {
        let (authority, path) = url.split_once(':')?;
        if authority.contains('/') {
            return None;
        }
        (authority.rsplit('@').next()?, path)
    };
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    if host.is_empty()
        || !path.contains('/')
        || path
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
    {
        return None;
    }
    if host.contains(['?', '#', ' ', '\\']) || path.contains(['?', '#']) {
        return None;
    }
    Some(format!("{}/{path}", host.to_ascii_lowercase()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_network_remotes_without_losing_namespaces() {
        for url in [
            "git@Code.Example.com:group/subgroup/repo.git",
            "https://Code.Example.com/group/subgroup/repo.git",
            "ssh://git@Code.Example.com:2222/group/subgroup/repo.git",
        ] {
            assert_eq!(
                remote_slug(url).as_deref(),
                Some("code.example.com/group/subgroup/repo")
            );
        }
        assert_eq!(
            remote_slug("https://code.example.com:8443/team/repo").as_deref(),
            Some("code.example.com:8443/team/repo")
        );
        for url in [
            "/local/repo",
            "../repo",
            "file:///local/repo",
            "https://host/team/../repo",
        ] {
            assert_eq!(remote_slug(url), None);
        }
    }

    #[tokio::test]
    async fn discovers_origin_then_other_network_remotes() {
        let directory = std::env::temp_dir()
            .join(format!("prtui-remotes-{}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        assert_eq!(current_remote(&directory).await.unwrap(), None);
        for args in [
            vec!["init", "--quiet"],
            vec![
                "remote",
                "add",
                "upstream",
                "https://upstream.example/team/repo.git",
            ],
            vec![
                "remote",
                "add",
                "origin",
                "https://origin.example/team/repo.git",
            ],
        ] {
            assert!(
                Command::new("git")
                    .current_dir(&directory)
                    .args(args)
                    .status()
                    .await
                    .unwrap()
                    .success()
            );
        }
        assert_eq!(
            current_remote(&directory).await.unwrap().as_deref(),
            Some("origin.example/team/repo")
        );
        assert!(
            Command::new("git")
                .current_dir(&directory)
                .args(["remote", "set-url", "origin", "/local/repo"])
                .status()
                .await
                .unwrap()
                .success()
        );
        assert_eq!(
            current_remote(&directory).await.unwrap().as_deref(),
            Some("upstream.example/team/repo")
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn inconclusive_probe_does_not_persist_the_fallback() {
        let directory = std::env::temp_dir()
            .join(format!("prtui-fallback-{}", std::process::id()));
        let path = directory.join("hosts.json");
        assert_eq!(
            select("invalid host", None, Some(&path)).await.unwrap(),
            ProviderChoice::Github
        );
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn override_bypasses_invalid_saved_configuration() {
        let path = std::env::temp_dir()
            .join(format!("prtui-invalid-hosts-{}.json", std::process::id()));
        std::fs::write(&path, "invalid json").unwrap();
        assert_eq!(
            select("example.com", Some(ProviderChoice::Github), Some(&path))
                .await
                .unwrap(),
            ProviderChoice::Github
        );
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn known_hosts_are_persisted_and_saved_hosts_skip_probing() {
        let directory = std::env::temp_dir()
            .join(format!("prtui-hosts-{}", std::process::id()));
        let path = directory.join("hosts.json");
        select("GITHUB.COM", None, Some(&path)).await.unwrap();
        let mut hosts = read_hosts(Some(&path)).unwrap();
        assert_eq!(hosts.get("github.com").map(String::as_str), Some("github"));
        hosts.insert("unreachable.invalid".into(), "github".into());
        write_hosts(&path, &hosts).unwrap();
        assert_eq!(
            select("unreachable.invalid", None, Some(&path))
                .await
                .unwrap(),
            ProviderChoice::Github
        );
        std::fs::remove_dir_all(directory).unwrap();
    }
}
