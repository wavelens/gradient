/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use gradient_wire::traits::InputProgressSink;
use serde::Deserialize;
use std::sync::Arc;
use url::Url;

use super::lock::{GITHUB_HOST, GITLAB_HOST, LockedRef};
use super::narhash::{nar_hash_of_dir, tarball_source_nar_hash};

const USER_AGENT: &str = "gradient-flake-lock";
const GITHUB_API: &str = "https://api.github.com";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedRev {
    pub rev: String,
    pub ref_: Option<String>,
    pub nar_hash: String,
    pub last_modified: i64,
}

#[async_trait]
pub trait RevisionResolver: Send + Sync {
    async fn resolve(
        &self,
        reference: &LockedRef,
        progress: Arc<dyn InputProgressSink>,
    ) -> Result<ResolvedRev>;
}

pub struct HttpRevisionResolver {
    client: reqwest::Client,
    github_token: Option<String>,
    gitlab_token: Option<String>,
    ssh_key: Option<crate::git::SshGitPrivateKeyKnownHostsAndObservations>,
}

impl std::fmt::Debug for HttpRevisionResolver {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let redact = |o: &Option<String>| o.as_ref().map(|_| "<redacted>");
        f.debug_struct("HttpRevisionResolver")
            .field("github_token", &redact(&self.github_token))
            .field("gitlab_token", &redact(&self.gitlab_token))
            .field("ssh_key", &self.ssh_key)
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

    pub fn with_ssh_key(
        mut self,
        key: Option<crate::git::SshGitPrivateKeyKnownHostsAndObservations>,
    ) -> Self {
        self.ssh_key = key;
        self
    }

    async fn resolve_github(
        &self,
        owner: &str,
        repo: &str,
        ref_: &Option<String>,
        progress: &dyn InputProgressSink,
    ) -> Result<ResolvedRev> {
        let r = ref_.clone().unwrap_or_else(|| "HEAD".to_owned());
        let github = Url::parse(GITHUB_API).context("Parse a URL for the GitHub API")?;
        let url = url_with_percent_encoded_path_segments(
            &github,
            &["repos", owner, repo, "commits", &r],
        )?;
        let commit: GithubCommit = self
            .github_get(url)
            .send()
            .await
            .context("github commit lookup")?
            .error_for_status()
            .context("look up the github commit")?
            .json()
            .await
            .context("parse github commit")?;

        let last_modified = parse_rfc3339(&commit.commit.committer.date)?;
        let tarball = url_with_percent_encoded_path_segments(
            &github,
            &["repos", owner, repo, "tarball", &commit.sha],
        )?;
        let nar_hash = tarball_source_nar_hash(self.github_get(tarball), progress).await?;

        Ok(ResolvedRev {
            rev: commit.sha,
            ref_: ref_.clone(),
            nar_hash,
            last_modified,
        })
    }

    async fn resolve_gitlab(
        &self,
        host: &str,
        owner: &str,
        repo: &str,
        ref_: &Option<String>,
        progress: &dyn InputProgressSink,
    ) -> Result<ResolvedRev> {
        let project_api = gitlab_project_api(host, owner, repo)?;
        let r = ref_.clone().unwrap_or_else(|| "HEAD".to_owned());
        let url =
            url_with_percent_encoded_path_segments(&project_api, &["repository", "commits", &r])?;
        let commit: GitlabCommit = self
            .gitlab_get(url)
            .send()
            .await
            .context("gitlab commit lookup")?
            .error_for_status()
            .context("look up the gitlab commit")?
            .json()
            .await
            .context("parse gitlab commit")?;

        let last_modified = parse_rfc3339(&commit.committed_date)?;
        let mut tarball = url_with_percent_encoded_path_segments(
            &project_api,
            &["repository", "archive.tar.gz"],
        )?;
        tarball.query_pairs_mut().append_pair("sha", &commit.id);
        let nar_hash = tarball_source_nar_hash(self.gitlab_get(tarball), progress).await?;

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
            git_checkout(&url, ref_owned.as_deref(), ssh_key.as_ref())
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

    fn github_get(&self, url: Url) -> reqwest::RequestBuilder {
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

    fn gitlab_get(&self, url: Url) -> reqwest::RequestBuilder {
        let mut req = self.client.get(url).header("User-Agent", USER_AGENT);
        if let Some(token) = &self.gitlab_token {
            req = req.header("PRIVATE-TOKEN", token);
        }

        req
    }
}

#[async_trait]
impl RevisionResolver for HttpRevisionResolver {
    async fn resolve(
        &self,
        reference: &LockedRef,
        progress: Arc<dyn InputProgressSink>,
    ) -> Result<ResolvedRev> {
        match reference {
            LockedRef::Github {
                owner,
                repo,
                host,
                ref_,
            } => {
                if let Some(host) = host.as_deref().filter(|host| *host != GITHUB_HOST) {
                    bail!("the updater doesn't support GitHub inputs on {host} yet");
                }
                self.resolve_github(owner, repo, ref_, &*progress).await
            }
            LockedRef::Gitlab {
                owner,
                repo,
                host,
                ref_,
            } => {
                let host = host.as_deref().unwrap_or(GITLAB_HOST);
                self.resolve_gitlab(host, owner, repo, ref_, &*progress)
                    .await
            }
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

fn gitlab_project_api(host: &str, owner: &str, repo: &str) -> Result<Url> {
    let project = format!("{}/{repo}", owner.replace("%2F", "/"));
    url_with_percent_encoded_path_segments(
        &gitlab_server_https_base_url_from_flake_lock(host)?,
        &["api", "v4", "projects", &project],
    )
}

fn gitlab_server_https_base_url_from_flake_lock(host: &str) -> Result<Url> {
    let mut url = Url::parse("https://gitlab.com").context("Parse a GitLab URL")?;
    url.set_host(Some(host))
        .with_context(|| format!("Invalid GitLab host `{host}`"))?;
    Ok(url)
}

fn url_with_percent_encoded_path_segments(base: &Url, segments: &[&str]) -> Result<Url> {
    let mut url = base.clone();
    url.path_segments_mut()
        .map_err(|()| anyhow::anyhow!("{base} cannot have a path."))?
        .pop_if_empty()
        .extend(segments);
    Ok(url)
}

fn git_checkout(
    url: &str,
    ref_: Option<&str>,
    ssh_key: Option<&crate::git::SshGitPrivateKeyKnownHostsAndObservations>,
) -> Result<(String, i64, tempfile::TempDir)> {
    let rev = crate::git::remote_ref_commit(url, ref_, ssh_key)
        .with_context(|| format!("Resolve git ref `{}` for {url}", ref_.unwrap_or("HEAD")))?;
    let tmp = crate::git::checkout_commit(url, &rev, ssh_key)?;
    let last_modified = commit_time(tmp.path(), &rev)?;
    std::fs::remove_dir_all(tmp.path().join(".git")).ok();

    Ok((rev, last_modified, tmp))
}

fn commit_time(checkout: &std::path::Path, rev: &str) -> Result<i64> {
    let repo = git2::Repository::open(checkout).context("open the checkout")?;
    let commit = repo.find_commit(git2::Oid::from_str(rev)?)?;
    Ok(commit.time().seconds())
}

fn parse_rfc3339(s: &str) -> Result<i64> {
    Ok(chrono::DateTime::parse_from_rfc3339(s)
        .with_context(|| format!("parse commit date `{s}`"))?
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
    use std::path::Path;
    use std::process::Command;

    fn git(repo: &Path, args: &[&str]) -> String {
        let out = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    fn commit_file(repo: &Path, name: &str) {
        std::fs::write(repo.join(name), name).unwrap();
        git(repo, &["add", "."]);
        git(repo, &["commit", "-m", name]);
    }

    fn make_git_repo() -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let repo = tmp.path().join("r");
        std::fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-b", "main"]);
        commit_file(&repo, "f");
        (tmp, repo)
    }

    // `git_checkout` is called directly because a reqwest client cannot be built in the sandboxed
    // CI without system CA certificates.
    #[test]
    fn a_gitlab_input_is_resolved_through_the_projects_api_of_its_host() {
        assert_eq!(
            gitlab_project_api("gitlab.example.com", "group%2Fsub", "tool")
                .unwrap()
                .as_str(),
            "https://gitlab.example.com/api/v4/projects/group%2Fsub%2Ftool"
        );
    }

    #[test]
    fn flake_lock_values_stay_inside_their_url_path_segment() {
        let github = Url::parse(GITHUB_API).unwrap();
        let url = url_with_percent_encoded_path_segments(
            &github,
            &["repos", "owner", "repo", "commits", "feature/x?y#z"],
        )
        .unwrap();
        assert_eq!(
            url.as_str(),
            "https://api.github.com/repos/owner/repo/commits/feature%2Fx%3Fy%23z"
        );
    }

    #[test]
    fn a_gitlab_host_with_a_path_is_rejected() {
        assert!(gitlab_project_api("evil.example.com/x", "group", "tool").is_err());
    }

    #[test]
    fn git_checkout_fetches_file_url() {
        let (_tmp, repo) = make_git_repo();
        let url = format!("file://{}", repo.display());
        let (rev, _, checkout) = git_checkout(&url, None, None).unwrap();
        assert_eq!(rev, git(&repo, &["rev-parse", "main"]));
        assert!(checkout.path().join("f").exists());
        assert!(!checkout.path().join(".git").exists());
    }

    #[test]
    fn git_checkout_resolves_a_branch_other_than_the_default() {
        let (_tmp, repo) = make_git_repo();
        git(&repo, &["checkout", "-b", "feature"]);
        commit_file(&repo, "g");
        git(&repo, &["checkout", "main"]);
        let url = format!("file://{}", repo.display());

        let (rev, last_modified, checkout) = git_checkout(&url, Some("feature"), None).unwrap();

        assert_eq!(rev, git(&repo, &["rev-parse", "feature"]));
        assert_eq!(
            last_modified.to_string(),
            git(&repo, &["log", "-1", "--format=%ct", "feature"])
        );
        assert!(checkout.path().join("g").exists());
    }

    #[test]
    fn git_checkout_with_ssh_key_still_clones_file_url() {
        let (_tmp, repo) = make_git_repo();
        let url = format!("file://{}", repo.display());
        let ssh = crate::git::SshGitPrivateKeyKnownHostsAndObservations::new(
            "-----BEGIN OPENSSH PRIVATE KEY-----\n".into(),
            Vec::new(),
        );
        let out = git_checkout(&url, None, Some(&ssh));
        assert!(
            out.is_ok(),
            "file:// clone with an ssh key failed: {:?}",
            out.err()
        );
    }
}
