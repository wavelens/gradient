/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Forge-agnostic push event parsing and triggering.

use serde::Deserialize;
use tracing::warn;

// ── GitHub push payload ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitHubPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    pub repository: GitHubRepository,
}

#[derive(Deserialize)]
pub(super) struct GitHubRepository {
    pub clone_url: String,
    pub ssh_url: String,
    #[serde(default)]
    pub full_name: Option<String>,
}

// ── Gitea/Forgejo push payload ─────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GiteaPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    pub repository: GiteaRepository,
}

#[derive(Deserialize)]
pub(super) struct GiteaRepository {
    pub clone_url: String,
    pub ssh_url: Option<String>,
    #[serde(default)]
    pub full_name: Option<String>,
}

// ── GitLab push payload ────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitLabPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    pub project: GitLabProject,
}

#[derive(Deserialize)]
pub(super) struct GitLabProject {
    pub http_url: String,
    pub ssh_url: Option<String>,
}

// ── GitHub PR payload ──────────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitHubPullRequestPayload {
    pub action: String,
    pub pull_request: GitHubPullRequest,
    pub repository: GitHubRepository,
}

#[derive(Deserialize)]
pub(super) struct GitHubPullRequest {
    pub head: GitHubPRRef,
    #[serde(default)]
    pub base: Option<GitHubPRRef>,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub user: Option<GitHubUser>,
}

#[derive(Deserialize)]
pub(super) struct GitHubPRRef {
    pub sha: String,
    #[serde(rename = "ref")]
    pub branch: String,
    #[serde(default)]
    pub repo: Option<GitHubRepoStub>,
}

#[derive(Deserialize)]
pub(super) struct GitHubRepoStub {
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub clone_url: Option<String>,
    #[serde(default)]
    pub ssh_url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GitHubUser {
    #[serde(default)]
    pub login: Option<String>,
}

// ── GitHub release payload ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitHubReleasePayload {
    pub release: GitHubRelease,
    pub repository: GitHubRepository,
}

#[derive(Deserialize)]
pub(super) struct GitHubRelease {
    pub tag_name: String,
    pub target_commitish: String,
}

// ── Gitea/Forgejo PR payload ───────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GiteaPullRequestPayload {
    pub action: String,
    pub pull_request: GiteaPullRequest,
    pub repository: GiteaRepository,
}

#[derive(Deserialize)]
pub(super) struct GiteaPullRequest {
    pub head: GiteaPRRef,
    #[serde(default)]
    pub base: Option<GiteaPRRef>,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub user: Option<GiteaUser>,
}

#[derive(Deserialize)]
pub(super) struct GiteaPRRef {
    pub sha: String,
    #[serde(rename = "ref")]
    pub branch: Option<String>,
    pub name: Option<String>,
    #[serde(default)]
    pub repo: Option<GiteaRepoStub>,
}

#[derive(Deserialize)]
pub(super) struct GiteaRepoStub {
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub clone_url: Option<String>,
    #[serde(default)]
    pub ssh_url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GiteaUser {
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub login: Option<String>,
}

// ── Gitea/Forgejo release payload ─────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GiteaReleasePayload {
    pub release: GiteaRelease,
    pub repository: GiteaRepository,
}

#[derive(Deserialize)]
pub(super) struct GiteaRelease {
    pub tag_name: String,
    pub target_commitish: Option<String>,
    pub sha: Option<String>,
}

// ── GitLab merge_request payload ───────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitLabMergeRequestPayload {
    pub object_attributes: GitLabMRAttributes,
    pub project: GitLabProject,
    #[serde(default)]
    pub user: Option<GitLabUser>,
}

#[derive(Deserialize)]
pub(super) struct GitLabMRAttributes {
    pub action: String,
    pub source_branch: String,
    pub last_commit: GitLabCommit,
    #[serde(default)]
    pub iid: Option<u64>,
    #[serde(default)]
    pub source_project_id: Option<u64>,
    #[serde(default)]
    pub target_project_id: Option<u64>,
    #[serde(default)]
    pub source: Option<GitLabMRSource>,
}

#[derive(Deserialize)]
pub(super) struct GitLabMRSource {
    #[serde(default)]
    pub git_http_url: Option<String>,
    #[serde(default)]
    pub git_ssh_url: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GitLabUser {
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Deserialize)]
pub(super) struct GitLabCommit {
    pub id: String,
}

// ── GitLab release payload ─────────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitLabReleasePayload {
    pub project: GitLabProject,
    pub commit: Option<GitLabCommit>,
    pub tag: Option<String>,
}

// ── Normalised push event ──────────────────────────────────────────────────

/// Forge-agnostic push event extracted from any of the supported webhook
/// payload shapes.
pub(super) struct ParsedPushEvent {
    pub commit_hash: Vec<u8>,
    pub repository_urls: Vec<String>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
    /// Branch name extracted from `refs/heads/<branch>`, or tag name from
    /// `refs/tags/<tag>`. Always present for valid pushes.
    pub ref_name: String,
    /// Whether the ref is a tag (`refs/tags/…`). False for branch pushes.
    pub is_tag: bool,
}

/// Pull-request event normalised across forges. `commit_hash` is the PR head SHA.
pub(super) struct ParsedPullRequestEvent {
    pub commit_hash: Vec<u8>,
    pub repository_urls: Vec<String>,
    /// Forge-reported action: "opened", "synchronize", "reopened", "closed", "merged", etc.
    pub action: String,
    /// PR head branch name (without `refs/heads/` prefix), if available.
    pub branch: Option<String>,
    /// PR / MR number as the forge knows it (GitHub `pull_request.number`,
    /// Gitea/Forgejo `pull_request.number`, GitLab `object_attributes.iid`).
    /// `None` when the payload omits it.
    pub pr_number: Option<u64>,
    /// Login/username of the PR author.
    pub pr_author: Option<String>,
    /// `true` when the head repo is not the base repo (i.e. the PR comes
    /// from a fork). `false` when same-repo. `None` when the payload lacks
    /// enough information to decide - callers should treat as untrusted.
    pub is_fork: Option<bool>,
    /// Clone URL of the PR head repo when the PR is from a fork. Used by
    /// `apply_trigger` to override the evaluation's `repository` field so
    /// the worker fetches the commit from the fork (where it actually
    /// exists) rather than the base repo. `None` for same-repo PRs and
    /// for payloads that lack a `head.repo.clone_url`.
    pub head_repo_clone_url: Option<String>,
}

/// Release/tag event. `commit_hash` is the SHA the tag points at.
pub(super) struct ParsedReleaseEvent {
    pub commit_hash: Vec<u8>,
    pub repository_urls: Vec<String>,
    /// Tag name (e.g. "v1.2.3"), if available.
    pub tag: Option<String>,
}

// ── Helpers ────────────────────────────────────────────────────────────────

/// Validated push commit info.
pub(super) struct PushCommit {
    pub hash: Vec<u8>,
    pub ref_name: String,
    pub is_tag: bool,
}

/// Validates a push event ref/SHA pair and decodes the commit hash.
///
/// Returns `None` for branch/tag deletions (all-zero SHA) or unparseable hex.
/// Handles both `refs/heads/<branch>` and `refs/tags/<tag>`.
pub(super) fn decode_push_commit(git_ref: &str, after: &str, forge: &str) -> Option<PushCommit> {
    if after == "0000000000000000000000000000000000000000" {
        return None;
    }
    let (ref_name, is_tag) = if let Some(branch) = git_ref.strip_prefix("refs/heads/") {
        (branch.to_string(), false)
    } else if let Some(tag) = git_ref.strip_prefix("refs/tags/") {
        (tag.to_string(), true)
    } else {
        return None;
    };
    match hex::decode(after) {
        Ok(hash) => Some(PushCommit {
            hash,
            ref_name,
            is_tag,
        }),
        Err(e) => {
            warn!(error = %e, sha = %after, forge, "Push webhook: invalid commit SHA");
            None
        }
    }
}

fn decode_sha_hex(s: &str, forge: &str, context: &str) -> Option<Vec<u8>> {
    if s.len() != 40 {
        return None;
    }
    match hex::decode(s) {
        Ok(b) => Some(b),
        Err(e) => {
            warn!(error = %e, sha = %s, forge, context, "invalid SHA");
            None
        }
    }
}

/// Normalise GitLab MR action strings to GitHub vocabulary.
fn normalise_gitlab_mr_action(action: &str) -> String {
    match action {
        "open" => "opened",
        "update" => "synchronize",
        "reopen" => "reopened",
        "merge" => "merged",
        "close" => "closed",
        other => other,
    }
    .to_string()
}

fn gitea_repo_urls(repo: &GiteaRepository) -> Vec<String> {
    let mut urls = vec![repo.clone_url.clone()];
    if let Some(ssh) = &repo.ssh_url {
        urls.push(ssh.clone());
    }
    urls
}

fn gitlab_project_urls(project: &GitLabProject) -> Vec<String> {
    let mut urls = vec![project.http_url.clone()];
    if let Some(ssh) = &project.ssh_url {
        urls.push(ssh.clone());
    }
    urls
}

// ── ParsedPushEvent impl ───────────────────────────────────────────────────

impl ParsedPushEvent {
    pub fn from_github(body: &[u8]) -> Option<Self> {
        let payload: GitHubPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitHub push payload");
                return None;
            }
        };
        let pc = decode_push_commit(&payload.git_ref, &payload.after, "github")?;
        Some(Self {
            commit_hash: pc.hash,
            repository_urls: vec![payload.repository.clone_url, payload.repository.ssh_url],
            commit_message: None,
            author_name: None,
            ref_name: pc.ref_name,
            is_tag: pc.is_tag,
        })
    }

    pub fn from_gitea(body: &[u8]) -> Option<Self> {
        let payload: GiteaPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse Gitea/Forgejo push payload");
                return None;
            }
        };
        let pc = decode_push_commit(&payload.git_ref, &payload.after, "gitea")?;
        let mut urls = vec![payload.repository.clone_url];
        if let Some(ssh) = payload.repository.ssh_url {
            urls.push(ssh);
        }
        Some(Self {
            commit_hash: pc.hash,
            repository_urls: urls,
            commit_message: None,
            author_name: None,
            ref_name: pc.ref_name,
            is_tag: pc.is_tag,
        })
    }

    pub fn from_gitlab(body: &[u8]) -> Option<Self> {
        let payload: GitLabPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitLab push payload");
                return None;
            }
        };
        let pc = decode_push_commit(&payload.git_ref, &payload.after, "gitlab")?;
        let mut urls = vec![payload.project.http_url];
        if let Some(ssh) = payload.project.ssh_url {
            urls.push(ssh);
        }
        Some(Self {
            commit_hash: pc.hash,
            repository_urls: urls,
            commit_message: None,
            author_name: None,
            ref_name: pc.ref_name,
            is_tag: pc.is_tag,
        })
    }
}

// ── ParsedPullRequestEvent impl ────────────────────────────────────────────

impl ParsedPullRequestEvent {
    pub fn from_github(body: &[u8]) -> Option<Self> {
        let payload: GitHubPullRequestPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitHub pull_request payload");
                return None;
            }
        };
        let commit_hash = decode_sha_hex(
            &payload.pull_request.head.sha,
            "github",
            "pull_request.head.sha",
        )?;
        let base_full = payload
            .pull_request
            .base
            .as_ref()
            .and_then(|b| b.repo.as_ref())
            .and_then(|r| r.full_name.clone())
            .or_else(|| payload.repository.full_name.clone());
        let head_repo = payload.pull_request.head.repo.as_ref();
        let head_full = head_repo.and_then(|r| r.full_name.clone());
        let is_fork = match (head_full.as_deref(), base_full.as_deref()) {
            (Some(h), Some(b)) => Some(h != b),
            _ => None,
        };
        let pr_author = payload
            .pull_request
            .user
            .as_ref()
            .and_then(|u| u.login.clone());
        let mut repository_urls = Vec::with_capacity(4);
        let mut head_repo_clone_url: Option<String> = None;
        if let (Some(true), Some(repo)) = (is_fork, head_repo) {
            if let Some(url) = repo.clone_url.clone() {
                head_repo_clone_url = Some(url.clone());
                repository_urls.push(url);
            }
            if let Some(url) = repo.ssh_url.clone() { repository_urls.push(url); }
        }
        repository_urls.push(payload.repository.clone_url);
        repository_urls.push(payload.repository.ssh_url);
        Some(Self {
            commit_hash,
            repository_urls,
            action: payload.action,
            branch: Some(payload.pull_request.head.branch),
            pr_number: payload.pull_request.number,
            pr_author,
            is_fork,
            head_repo_clone_url,
        })
    }

    pub fn from_gitea(body: &[u8]) -> Option<Self> {
        let payload: GiteaPullRequestPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse Gitea/Forgejo pull_request payload");
                return None;
            }
        };
        let commit_hash = decode_sha_hex(
            &payload.pull_request.head.sha,
            "gitea",
            "pull_request.head.sha",
        )?;
        let branch = payload
            .pull_request
            .head
            .branch
            .clone()
            .or(payload.pull_request.head.name.clone());
        let base_full = payload
            .pull_request
            .base
            .as_ref()
            .and_then(|b| b.repo.as_ref())
            .and_then(|r| r.full_name.clone())
            .or_else(|| payload.repository.full_name.clone());
        let head_repo = payload.pull_request.head.repo.as_ref();
        let head_full = head_repo.and_then(|r| r.full_name.clone());
        let is_fork = match (head_full.as_deref(), base_full.as_deref()) {
            (Some(h), Some(b)) => Some(h != b),
            _ => None,
        };
        let pr_author = payload
            .pull_request
            .user
            .as_ref()
            .and_then(|u| u.username.clone().or_else(|| u.login.clone()));
        let mut repository_urls = Vec::with_capacity(4);
        let mut head_repo_clone_url: Option<String> = None;
        if let (Some(true), Some(repo)) = (is_fork, head_repo) {
            if let Some(url) = repo.clone_url.clone() {
                head_repo_clone_url = Some(url.clone());
                repository_urls.push(url);
            }
            if let Some(url) = repo.ssh_url.clone() { repository_urls.push(url); }
        }
        repository_urls.extend(gitea_repo_urls(&payload.repository));
        Some(Self {
            commit_hash,
            repository_urls,
            action: payload.action,
            branch,
            pr_number: payload.pull_request.number,
            pr_author,
            is_fork,
            head_repo_clone_url,
        })
    }

    pub fn from_gitlab(body: &[u8]) -> Option<Self> {
        let payload: GitLabMergeRequestPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitLab merge_request payload");
                return None;
            }
        };
        let commit_hash = decode_sha_hex(
            &payload.object_attributes.last_commit.id,
            "gitlab",
            "object_attributes.last_commit.id",
        )?;
        let action = normalise_gitlab_mr_action(&payload.object_attributes.action);
        let is_fork = match (
            payload.object_attributes.source_project_id,
            payload.object_attributes.target_project_id,
        ) {
            (Some(src), Some(tgt)) => Some(src != tgt),
            _ => None,
        };
        let pr_author = payload.user.as_ref().and_then(|u| u.username.clone());
        let mut repository_urls = Vec::with_capacity(4);
        let mut head_repo_clone_url: Option<String> = None;
        if let (Some(true), Some(src)) = (is_fork, payload.object_attributes.source.as_ref()) {
            if let Some(url) = src.git_http_url.clone() {
                head_repo_clone_url = Some(url.clone());
                repository_urls.push(url);
            }
            if let Some(url) = src.git_ssh_url.clone() { repository_urls.push(url); }
        }
        repository_urls.extend(gitlab_project_urls(&payload.project));
        Some(Self {
            commit_hash,
            repository_urls,
            action,
            branch: Some(payload.object_attributes.source_branch),
            pr_number: payload.object_attributes.iid,
            pr_author,
            is_fork,
            head_repo_clone_url,
        })
    }
}

// ── ParsedReleaseEvent impl ────────────────────────────────────────────────

impl ParsedReleaseEvent {
    pub fn from_github(body: &[u8]) -> Option<Self> {
        let payload: GitHubReleasePayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitHub release payload");
                return None;
            }
        };
        let commit_hash = decode_sha_hex(
            &payload.release.target_commitish,
            "github",
            "release.target_commitish",
        )?;
        Some(Self {
            commit_hash,
            repository_urls: vec![payload.repository.clone_url, payload.repository.ssh_url],
            tag: Some(payload.release.tag_name),
        })
    }

    pub fn from_gitea(body: &[u8]) -> Option<Self> {
        let payload: GiteaReleasePayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse Gitea/Forgejo release payload");
                return None;
            }
        };
        let commit_hash = payload
            .release
            .sha
            .as_deref()
            .and_then(|s| decode_sha_hex(s, "gitea", "release.sha"))
            .or_else(|| {
                payload
                    .release
                    .target_commitish
                    .as_deref()
                    .and_then(|s| decode_sha_hex(s, "gitea", "release.target_commitish"))
            })?;
        Some(Self {
            commit_hash,
            repository_urls: gitea_repo_urls(&payload.repository),
            tag: Some(payload.release.tag_name),
        })
    }

    pub fn from_gitlab(body: &[u8]) -> Option<Self> {
        let payload: GitLabReleasePayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitLab release payload");
                return None;
            }
        };
        let commit_hash = payload
            .commit
            .as_ref()
            .and_then(|c| decode_sha_hex(&c.id, "gitlab", "commit.id"))?;
        Some(Self {
            commit_hash,
            repository_urls: gitlab_project_urls(&payload.project),
            tag: payload.tag,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";
    const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

    // ── push helpers ──────────────────────────────────────────────────────

    #[test]
    fn decode_push_commit_accepts_branch_ref() {
        let out = decode_push_commit("refs/heads/main", VALID_SHA, "github").unwrap();
        assert_eq!(out.hash, hex::decode(VALID_SHA).unwrap());
        assert_eq!(out.ref_name, "main");
        assert!(!out.is_tag);
    }

    #[test]
    fn decode_push_commit_accepts_tag_ref() {
        let out = decode_push_commit("refs/tags/v1.0.0", VALID_SHA, "github").unwrap();
        assert_eq!(out.hash, hex::decode(VALID_SHA).unwrap());
        assert_eq!(out.ref_name, "v1.0.0");
        assert!(out.is_tag);
    }

    #[test]
    fn decode_push_commit_rejects_zero_sha_branch_deletion() {
        assert!(decode_push_commit("refs/heads/main", ZERO_SHA, "github").is_none());
    }

    #[test]
    fn decode_push_commit_rejects_invalid_hex() {
        assert!(decode_push_commit("refs/heads/main", "not-hex-at-all", "github").is_none());
    }

    #[test]
    fn decode_push_commit_rejects_empty_ref() {
        assert!(decode_push_commit("", VALID_SHA, "github").is_none());
    }

    // ── GitHub PR ─────────────────────────────────────────────────────────

    #[test]
    fn parse_github_pr_opened_event() {
        let body = format!(
            r#"{{
                "action": "opened",
                "pull_request": {{
                    "head": {{
                        "sha": "{VALID_SHA}",
                        "ref": "feature-x"
                    }}
                }},
                "repository": {{
                    "clone_url": "https://github.com/org/repo.git",
                    "ssh_url": "git@github.com:org/repo.git"
                }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_github(body.as_bytes()).unwrap();
        assert_eq!(ev.action, "opened");
        assert_eq!(ev.branch, Some("feature-x".to_string()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
        assert_eq!(ev.repository_urls.len(), 2);
    }

    #[test]
    fn github_pr_from_fork_marks_is_fork_and_extracts_metadata() {
        let body = format!(
            r#"{{
                "action": "opened",
                "pull_request": {{
                    "number": 42,
                    "user": {{ "login": "external-contrib" }},
                    "head": {{
                        "sha": "{VALID_SHA}",
                        "ref": "patch-1",
                        "repo": {{ "full_name": "external-contrib/repo" }}
                    }},
                    "base": {{
                        "sha": "0000000000000000000000000000000000000000",
                        "ref": "main",
                        "repo": {{ "full_name": "org/repo" }}
                    }}
                }},
                "repository": {{
                    "clone_url": "https://github.com/org/repo.git",
                    "ssh_url": "git@github.com:org/repo.git",
                    "full_name": "org/repo"
                }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_github(body.as_bytes()).unwrap();
        assert_eq!(ev.pr_number, Some(42));
        assert_eq!(ev.pr_author.as_deref(), Some("external-contrib"));
        assert_eq!(ev.is_fork, Some(true));
    }

    #[test]
    fn github_pr_same_repo_is_not_fork() {
        let body = format!(
            r#"{{
                "action": "synchronize",
                "pull_request": {{
                    "number": 7,
                    "user": {{ "login": "maintainer" }},
                    "head": {{
                        "sha": "{VALID_SHA}",
                        "ref": "feature",
                        "repo": {{ "full_name": "org/repo" }}
                    }},
                    "base": {{
                        "sha": "0000000000000000000000000000000000000000",
                        "ref": "main",
                        "repo": {{ "full_name": "org/repo" }}
                    }}
                }},
                "repository": {{
                    "clone_url": "https://github.com/org/repo.git",
                    "ssh_url": "git@github.com:org/repo.git",
                    "full_name": "org/repo"
                }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_github(body.as_bytes()).unwrap();
        assert_eq!(ev.is_fork, Some(false));
    }

    #[test]
    fn gitlab_mr_different_projects_marks_is_fork() {
        let body = format!(
            r#"{{
                "object_attributes": {{
                    "action": "open",
                    "iid": 11,
                    "source_branch": "feat",
                    "source_project_id": 99,
                    "target_project_id": 1,
                    "last_commit": {{ "id": "{VALID_SHA}" }}
                }},
                "project": {{
                    "http_url": "https://gitlab.example.com/group/repo.git",
                    "ssh_url": "git@gitlab.example.com:group/repo.git",
                    "path_with_namespace": "group/repo"
                }},
                "user": {{ "username": "external" }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_gitlab(body.as_bytes()).unwrap();
        assert_eq!(ev.pr_number, Some(11));
        assert_eq!(ev.pr_author.as_deref(), Some("external"));
        assert_eq!(ev.is_fork, Some(true));
    }

    #[test]
    fn parse_github_pr_invalid_sha_returns_none() {
        let body = r#"{
            "action": "opened",
            "pull_request": { "head": { "sha": "not-a-sha", "ref": "feature-x" } },
            "repository": { "clone_url": "https://github.com/org/repo.git", "ssh_url": "git@github.com:org/repo.git" }
        }"#;
        assert!(ParsedPullRequestEvent::from_github(body.as_bytes()).is_none());
    }

    // ── Gitea PR ──────────────────────────────────────────────────────────

    #[test]
    fn parse_gitea_pr_opened_event() {
        let body = format!(
            r#"{{
                "action": "opened",
                "pull_request": {{
                    "head": {{
                        "sha": "{VALID_SHA}",
                        "ref": "feature-y",
                        "name": "feature-y"
                    }}
                }},
                "repository": {{
                    "clone_url": "https://gitea.example.com/org/repo.git",
                    "ssh_url": "git@gitea.example.com:org/repo.git"
                }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_gitea(body.as_bytes()).unwrap();
        assert_eq!(ev.action, "opened");
        assert_eq!(ev.branch, Some("feature-y".to_string()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
    }

    #[test]
    fn parse_gitea_pr_falls_back_to_name_field_for_branch() {
        let body = format!(
            r#"{{
                "action": "synchronize",
                "pull_request": {{
                    "head": {{
                        "sha": "{VALID_SHA}",
                        "name": "fallback-branch"
                    }}
                }},
                "repository": {{
                    "clone_url": "https://gitea.example.com/org/repo.git"
                }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_gitea(body.as_bytes()).unwrap();
        assert_eq!(ev.branch, Some("fallback-branch".to_string()));
    }

    // ── GitLab MR ─────────────────────────────────────────────────────────

    #[test]
    fn parse_gitlab_mr_open_normalised_to_opened() {
        let body = format!(
            r#"{{
                "object_attributes": {{
                    "action": "open",
                    "source_branch": "feature-z",
                    "last_commit": {{ "id": "{VALID_SHA}" }}
                }},
                "project": {{
                    "http_url": "https://gitlab.example.com/org/repo.git",
                    "ssh_url": "git@gitlab.example.com:org/repo.git"
                }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_gitlab(body.as_bytes()).unwrap();
        assert_eq!(ev.action, "opened");
        assert_eq!(ev.branch, Some("feature-z".to_string()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
    }

    #[test]
    fn normalise_gitlab_mr_action_maps_all_variants() {
        assert_eq!(normalise_gitlab_mr_action("open"), "opened");
        assert_eq!(normalise_gitlab_mr_action("update"), "synchronize");
        assert_eq!(normalise_gitlab_mr_action("reopen"), "reopened");
        assert_eq!(normalise_gitlab_mr_action("merge"), "merged");
        assert_eq!(normalise_gitlab_mr_action("close"), "closed");
        assert_eq!(normalise_gitlab_mr_action("unknown"), "unknown");
    }

    // ── GitHub release ────────────────────────────────────────────────────

    #[test]
    fn parse_github_release_with_sha_target_commitish() {
        let body = format!(
            r#"{{
                "action": "published",
                "release": {{
                    "tag_name": "v1.2.3",
                    "target_commitish": "{VALID_SHA}"
                }},
                "repository": {{
                    "clone_url": "https://github.com/org/repo.git",
                    "ssh_url": "git@github.com:org/repo.git"
                }}
            }}"#
        );
        let ev = ParsedReleaseEvent::from_github(body.as_bytes()).unwrap();
        assert_eq!(ev.tag, Some("v1.2.3".to_string()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
    }

    #[test]
    fn parse_github_release_with_branch_target_commitish_returns_none() {
        let body = r#"{
            "action": "published",
            "release": {
                "tag_name": "v1.2.3",
                "target_commitish": "main"
            },
            "repository": {
                "clone_url": "https://github.com/org/repo.git",
                "ssh_url": "git@github.com:org/repo.git"
            }
        }"#;
        assert!(ParsedReleaseEvent::from_github(body.as_bytes()).is_none());
    }

    // ── Gitea release ─────────────────────────────────────────────────────

    #[test]
    fn parse_gitea_release_with_sha_field() {
        let body = format!(
            r#"{{
                "action": "published",
                "release": {{
                    "tag_name": "v2.0.0",
                    "sha": "{VALID_SHA}",
                    "target_commitish": "main"
                }},
                "repository": {{
                    "clone_url": "https://gitea.example.com/org/repo.git"
                }}
            }}"#
        );
        let ev = ParsedReleaseEvent::from_gitea(body.as_bytes()).unwrap();
        assert_eq!(ev.tag, Some("v2.0.0".to_string()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
    }

    #[test]
    fn parse_gitea_release_falls_back_to_target_commitish_sha() {
        let body = format!(
            r#"{{
                "action": "published",
                "release": {{
                    "tag_name": "v2.1.0",
                    "target_commitish": "{VALID_SHA}"
                }},
                "repository": {{
                    "clone_url": "https://gitea.example.com/org/repo.git"
                }}
            }}"#
        );
        let ev = ParsedReleaseEvent::from_gitea(body.as_bytes()).unwrap();
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
    }

    #[test]
    fn parse_gitea_release_returns_none_when_no_sha_available() {
        let body = r#"{
            "action": "published",
            "release": {
                "tag_name": "v2.2.0",
                "target_commitish": "main"
            },
            "repository": {
                "clone_url": "https://gitea.example.com/org/repo.git"
            }
        }"#;
        assert!(ParsedReleaseEvent::from_gitea(body.as_bytes()).is_none());
    }

    // ── GitLab release ────────────────────────────────────────────────────

    #[test]
    fn parse_gitlab_release_event() {
        let body = format!(
            r#"{{
                "action": "create",
                "project": {{
                    "http_url": "https://gitlab.example.com/org/repo.git",
                    "ssh_url": "git@gitlab.example.com:org/repo.git"
                }},
                "commit": {{ "id": "{VALID_SHA}" }},
                "tag": "v3.0.0"
            }}"#
        );
        let ev = ParsedReleaseEvent::from_gitlab(body.as_bytes()).unwrap();
        assert_eq!(ev.tag, Some("v3.0.0".to_string()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
    }

    #[test]
    fn parse_gitlab_release_without_commit_returns_none() {
        let body = r#"{
            "action": "create",
            "project": {
                "http_url": "https://gitlab.example.com/org/repo.git"
            },
            "tag": "v3.1.0"
        }"#;
        assert!(ParsedReleaseEvent::from_gitlab(body.as_bytes()).is_none());
    }
}
