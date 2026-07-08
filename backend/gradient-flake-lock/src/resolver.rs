/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Newest-revision resolution behind the [`RevisionResolver`] seam.
//!
//! [`HttpRevisionResolver`] resolves github/gitlab over each forge's HTTP API
//! and plain `git` via libgit2, recomputing `narHash` natively (see
//! [`crate::narhash`]). Unsupported fetcher types fail explicitly so a bad input
//! never produces a half-baked lock.

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use serde::Deserialize;

use crate::lock::LockedRef;
use crate::narhash::{nar_hash_of_dir, tarball_source_nar_hash};

const USER_AGENT: &str = "gradient-flake-lock";
const GITHUB_API: &str = "https://api.github.com";
const GITLAB_API: &str = "https://gitlab.com";

/// The newest revision of an input plus the metadata a `locked` block needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRev {
    pub rev: String,
    pub ref_: Option<String>,
    pub nar_hash: String,
    pub last_modified: i64,
}

/// Resolves the newest revision of a flake input. The seam that lets the
/// generator's rewrite logic be tested without network or nix.
#[async_trait]
pub trait RevisionResolver: Send + Sync {
    async fn resolve(&self, reference: &LockedRef) -> Result<ResolvedRev>;
}

/// Resolves github/gitlab over HTTP and plain git over libgit2.
pub struct HttpRevisionResolver {
    client: reqwest::Client,
    github_token: Option<String>,
    gitlab_token: Option<String>,
    ssh_key: Option<String>,
}

impl std::fmt::Debug for HttpRevisionResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |o: &Option<String>| o.as_ref().map(|_| "<redacted>");
        f.debug_struct("HttpRevisionResolver")
            .field("github_token", &redact(&self.github_token))
            .field("gitlab_token", &redact(&self.gitlab_token))
            .field("ssh_key", &redact(&self.ssh_key))
            .finish_non_exhaustive()
    }
}

impl HttpRevisionResolver {
    pub fn new(client: reqwest::Client) -> Self {
        Self {
            client,
            github_token: None,
            gitlab_token: None,
            ssh_key: None,
        }
    }

    pub fn with_github_token(mut self, token: Option<String>) -> Self {
        self.github_token = token;
        self
    }

    pub fn with_gitlab_token(mut self, token: Option<String>) -> Self {
        self.gitlab_token = token;
        self
    }

    pub fn with_ssh_key(mut self, key: Option<String>) -> Self {
        self.ssh_key = key;
        self
    }

    async fn resolve_github(
        &self,
        owner: &str,
        repo: &str,
        ref_: &Option<String>,
    ) -> Result<ResolvedRev> {
        let r = ref_.clone().unwrap_or_else(|| "HEAD".to_owned());
        let url = format!("{GITHUB_API}/repos/{owner}/{repo}/commits/{r}");
        let commit: GithubCommit = self
            .github_get(&url)
            .send()
            .await
            .context("github commit lookup")?
            .error_for_status()
            .context("github commit lookup failed")?
            .json()
            .await
            .context("parsing github commit")?;

        let last_modified = parse_rfc3339(&commit.commit.committer.date)?;
        let tarball = format!("{GITHUB_API}/repos/{owner}/{repo}/tarball/{}", commit.sha);
        let nar_hash = tarball_source_nar_hash(self.github_get(&tarball)).await?;

        Ok(ResolvedRev {
            rev: commit.sha,
            ref_: ref_.clone(),
            nar_hash,
            last_modified,
        })
    }

    async fn resolve_gitlab(
        &self,
        owner: &str,
        repo: &str,
        ref_: &Option<String>,
    ) -> Result<ResolvedRev> {
        let project = format!("{owner}/{repo}").replace('/', "%2F");
        let r = ref_.clone().unwrap_or_else(|| "HEAD".to_owned());
        let url = format!("{GITLAB_API}/api/v4/projects/{project}/repository/commits/{r}");
        let commit: GitlabCommit = self
            .gitlab_get(&url)
            .send()
            .await
            .context("gitlab commit lookup")?
            .error_for_status()
            .context("gitlab commit lookup failed")?
            .json()
            .await
            .context("parsing gitlab commit")?;

        let last_modified = parse_rfc3339(&commit.committed_date)?;
        let tarball = format!(
            "{GITLAB_API}/api/v4/projects/{project}/repository/archive.tar.gz?sha={}",
            commit.id
        );
        let nar_hash = tarball_source_nar_hash(self.gitlab_get(&tarball)).await?;

        Ok(ResolvedRev {
            rev: commit.id,
            ref_: ref_.clone(),
            nar_hash,
            last_modified,
        })
    }

    async fn resolve_git(&self, url: &str, ref_: &Option<String>) -> Result<ResolvedRev> {
        let url = url.to_owned();
        let ref_owned = ref_.clone();
        let ssh_key = self.ssh_key.clone();
        let (rev, last_modified, tmp) = tokio::task::spawn_blocking(move || {
            git_checkout(&url, ref_owned.as_deref(), ssh_key.as_deref())
        })
        .await
        .context("git checkout task panicked")??;
        let nar_hash = nar_hash_of_dir(tmp.path()).await?;

        Ok(ResolvedRev {
            rev,
            ref_: ref_.clone(),
            nar_hash,
            last_modified,
        })
    }

    fn github_get(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self
            .client
            .get(url)
            .header("User-Agent", USER_AGENT)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.github_token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }

        req
    }

    fn gitlab_get(&self, url: &str) -> reqwest::RequestBuilder {
        let mut req = self.client.get(url).header("User-Agent", USER_AGENT);
        if let Some(token) = &self.gitlab_token {
            req = req.header("PRIVATE-TOKEN", token);
        }

        req
    }
}

#[async_trait]
impl RevisionResolver for HttpRevisionResolver {
    async fn resolve(&self, reference: &LockedRef) -> Result<ResolvedRev> {
        match reference {
            LockedRef::Github { owner, repo, ref_ } => self.resolve_github(owner, repo, ref_).await,
            LockedRef::Gitlab { owner, repo, ref_ } => self.resolve_gitlab(owner, repo, ref_).await,
            LockedRef::Git { url, ref_ } => self.resolve_git(url, ref_).await,
            LockedRef::Tarball { .. } => {
                bail!("tarball inputs are not yet supported by the updater")
            }
            LockedRef::Sourcehut { .. } => bail!("sourcehut inputs are not yet supported"),
            LockedRef::Path { .. } => bail!("path inputs cannot be bumped"),
            LockedRef::Indirect { .. } => bail!("indirect inputs cannot be bumped"),
            LockedRef::Other(ty) => bail!("unsupported flake input type `{ty}`"),
        }
    }
}

/// Clone `url`, resolve `ref_` (default branch when `None`), and leave a clean
/// checkout (no `.git`) so its NAR matches nix's git-tree narHash.
fn git_checkout(
    url: &str,
    ref_: Option<&str>,
    ssh_key: Option<&str>,
) -> Result<(String, i64, tempfile::TempDir)> {
    let tmp = tempfile::tempdir().context("creating git temp dir")?;
    let repo = git2::build::RepoBuilder::new()
        .fetch_options(gradient_sources::fetch_options_with_ssh(ssh_key))
        .clone(url, tmp.path())
        .with_context(|| format!("cloning {url}"))?;

    let commit = match ref_ {
        Some(r) => repo
            .revparse_single(&format!("origin/{r}"))
            .or_else(|_| repo.revparse_single(r))
            .with_context(|| format!("resolving git ref `{r}`"))?
            .peel_to_commit()
            .context("ref does not point at a commit")?,
        None => repo
            .head()
            .context("reading HEAD")?
            .peel_to_commit()
            .context("HEAD commit")?,
    };

    let rev = commit.id().to_string();
    let last_modified = commit.time().seconds();
    let tree = commit.tree().context("commit tree")?;
    let mut checkout = git2::build::CheckoutBuilder::new();
    checkout.force();
    repo.checkout_tree(tree.as_object(), Some(&mut checkout))
        .context("git checkout")?;
    std::fs::remove_dir_all(tmp.path().join(".git")).ok();

    Ok((rev, last_modified, tmp))
}

fn parse_rfc3339(s: &str) -> Result<i64> {
    Ok(chrono::DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("parsing commit date `{s}`"))?
        .timestamp())
}

#[derive(Deserialize)]
struct GithubCommit {
    sha: String,
    commit: GithubCommitInner,
}

#[derive(Deserialize)]
struct GithubCommitInner {
    committer: GithubGitUser,
}

#[derive(Deserialize)]
struct GithubGitUser {
    date: String,
}

#[derive(Deserialize)]
struct GitlabCommit {
    id: String,
    committed_date: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn make_git_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("r");
        let rd = repo.to_str().unwrap();
        Command::new("git")
            .args(["init", rd, "-b", "main"])
            .output()
            .unwrap();
        std::fs::write(repo.join("f"), "x").unwrap();
        Command::new("git")
            .args(["-C", rd, "add", "."])
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "-C",
                rd,
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "-m",
                "i",
            ])
            .output()
            .unwrap();
        (tmp, repo)
    }

    // git_checkout is exercised directly (not via HttpRevisionResolver) so the
    // test needs no reqwest client, which cannot be built in the sandboxed CI
    // environment (no system CA certificates).
    #[test]
    fn git_checkout_clones_file_url_via_repobuilder() {
        let (_tmp, repo) = make_git_repo();
        let url = format!("file://{}", repo.display());
        let out = git_checkout(&url, None, None);
        assert!(out.is_ok(), "file:// clone failed: {:?}", out.err());
    }

    #[test]
    fn git_checkout_with_ssh_key_still_clones_file_url() {
        // An ssh key on the fetch options must not break a keyless (file://)
        // clone. Real ssh-auth behavior is covered by the shared helper + CI.
        let (_tmp, repo) = make_git_repo();
        let url = format!("file://{}", repo.display());
        let out = git_checkout(&url, None, Some("-----BEGIN OPENSSH PRIVATE KEY-----\n"));
        assert!(out.is_ok(), "keyed file:// clone failed: {:?}", out.err());
    }
}
