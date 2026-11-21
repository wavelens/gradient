/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::RepositoryUrl;
use serde::Deserialize;
use tracing::warn;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookEventKind {
    Push,
    PullRequest,
    Release,
    Comment,
    Review,
    Unknown(String),
}

#[derive(Deserialize)]
pub struct WebhookCommit {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub author: Option<WebhookCommitAuthor>,
}

#[derive(Deserialize)]
pub struct WebhookCommitAuthor {
    #[serde(default)]
    pub name: Option<String>,
}

fn commit_message(commit: &WebhookCommit) -> Option<String> {
    let message = commit.message.as_deref()?.trim();
    (!message.is_empty()).then(|| message.to_string())
}

/// `after` is the tag object for an annotated tag, and no fetch can resolve it as a commit. The Git
/// host's peeled SHA is winning.
fn head_commit_id(head: Option<&WebhookCommit>) -> Option<&str> {
    head.and_then(|c| c.id.as_deref())
}

#[derive(Deserialize)]
pub struct GitHubPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    pub repository: GitHubRepository,
    #[serde(default)]
    pub head_commit: Option<WebhookCommit>,
}

#[derive(Deserialize)]
pub struct GitHubRepository {
    pub clone_url: String,
    pub ssh_url: String,
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
}

#[derive(Deserialize)]
pub struct GitHubMergeGroupPayload {
    pub action: String,
    pub merge_group: GitHubMergeGroup,
    pub repository: GitHubRepository,
}

#[derive(Deserialize)]
pub struct GitHubMergeGroup {
    pub head_sha: String,
    pub head_ref: String,
    pub base_ref: String,
    #[serde(default)]
    pub head_commit: Option<WebhookCommit>,
}

#[derive(Deserialize)]
pub struct GiteaPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    pub repository: GiteaRepository,
    #[serde(default)]
    pub head_commit: Option<WebhookCommit>,
}

#[derive(Deserialize)]
pub struct GiteaRepository {
    pub clone_url: String,
    pub ssh_url: Option<String>,
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
}

#[derive(Deserialize)]
pub struct GitLabPushPayload {
    #[serde(rename = "ref")]
    pub git_ref: String,
    pub after: String,
    #[serde(default)]
    pub checkout_sha: Option<String>,
    pub project: GitLabProject,
    #[serde(default)]
    pub commits: Vec<WebhookCommit>,
}

#[derive(Deserialize)]
pub struct GitLabProject {
    pub http_url: String,
    pub ssh_url: Option<String>,
    #[serde(default)]
    pub default_branch: Option<String>,
}

#[derive(Deserialize)]
pub struct GitHubPullRequestPayload {
    pub action: String,
    pub pull_request: GitHubPullRequest,
    pub repository: GitHubRepository,
    #[serde(default)]
    pub sender: Option<GitHubUser>,
}

#[derive(Deserialize)]
pub struct GitHubPullRequest {
    pub head: GitHubPRRef,
    #[serde(default)]
    pub base: Option<GitHubPRRef>,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub user: Option<GitHubUser>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Deserialize)]
pub struct GitHubPRRef {
    pub sha: String,
    #[serde(rename = "ref")]
    pub branch: String,
    #[serde(default)]
    pub repo: Option<GitHubRepoStub>,
}

#[derive(Deserialize)]
pub struct GitHubRepoStub {
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub clone_url: Option<String>,
    #[serde(default)]
    pub ssh_url: Option<String>,
}

#[derive(Deserialize)]
pub struct GitHubUser {
    #[serde(default)]
    pub login: Option<String>,
}

#[derive(Deserialize)]
pub struct GitHubReleasePayload {
    pub release: GitHubRelease,
    pub repository: GitHubRepository,
}

#[derive(Deserialize)]
pub struct GitHubRelease {
    pub tag_name: String,
    pub target_commitish: String,
}

#[derive(Deserialize)]
pub struct GiteaPullRequestPayload {
    pub action: String,
    pub pull_request: GiteaPullRequest,
    pub repository: GiteaRepository,
    #[serde(default)]
    pub sender: Option<GiteaUser>,
}

#[derive(Deserialize)]
pub struct GiteaPullRequest {
    pub head: GiteaPRRef,
    #[serde(default)]
    pub base: Option<GiteaPRRef>,
    #[serde(default)]
    pub number: Option<u64>,
    #[serde(default)]
    pub user: Option<GiteaUser>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub flow: Option<i64>,
}

const FORGEJO_AGIT_FLOW: i64 = 1;

impl GiteaPullRequest {
    // Any reader can push an AGit head. Gitea marks it only by an empty label, Forgejo by `flow`.
    fn is_agit(&self) -> bool {
        let unlabelled_pull_ref = self.head.label.as_deref().unwrap_or_default().is_empty()
            && self
                .head
                .git_ref
                .as_deref()
                .is_some_and(|r| r.starts_with("refs/pull/"));
        self.flow == Some(FORGEJO_AGIT_FLOW) || unlabelled_pull_ref
    }

    pub fn head_branch(&self) -> Option<String> {
        self.head
            .label
            .clone()
            .filter(|label| !label.is_empty())
            .or_else(|| self.head.git_ref.clone())
    }

    pub fn is_fork(&self, base_repo_full_name: Option<&str>) -> Option<bool> {
        if self.is_agit() {
            return None;
        }

        let base_full = self
            .base
            .as_ref()
            .and_then(|b| b.repo.as_ref())
            .and_then(|r| r.full_name.as_deref())
            .or(base_repo_full_name);
        let head_full = self.head.repo.as_ref().and_then(|r| r.full_name.as_deref());
        match (head_full, base_full) {
            (Some(h), Some(b)) => Some(h != b),
            _ => None,
        }
    }

    pub fn fork_repo(&self, base_repo_full_name: Option<&str>) -> Option<&GiteaRepoStub> {
        match self.is_fork(base_repo_full_name) {
            Some(true) => self.head.repo.as_ref(),
            _ => None,
        }
    }
}

#[derive(Deserialize)]
pub struct GiteaPRRef {
    pub sha: String,
    #[serde(rename = "ref")]
    pub git_ref: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub repo: Option<GiteaRepoStub>,
}

#[derive(Deserialize)]
pub struct GiteaRepoStub {
    #[serde(default)]
    pub full_name: Option<String>,
    #[serde(default)]
    pub clone_url: Option<String>,
    #[serde(default)]
    pub ssh_url: Option<String>,
}

#[derive(Deserialize)]
pub struct GiteaUser {
    #[serde(default)]
    pub id: Option<i64>,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub login: Option<String>,
}

#[derive(Deserialize)]
pub struct GiteaReleasePayload {
    pub release: GiteaRelease,
    pub repository: GiteaRepository,
}

#[derive(Deserialize)]
pub struct GiteaRelease {
    pub tag_name: String,
    pub target_commitish: Option<String>,
    pub sha: Option<String>,
}

#[derive(Deserialize)]
pub struct GitLabMergeRequestPayload {
    pub object_attributes: GitLabMRAttributes,
    pub project: GitLabProject,
    #[serde(default)]
    pub user: Option<GitLabUser>,
}

#[derive(Deserialize)]
pub struct GitLabMRAttributes {
    pub action: String,
    pub source_branch: String,
    #[serde(default)]
    pub target_branch: Option<String>,
    pub last_commit: GitLabCommit,
    #[serde(default)]
    pub title: Option<String>,
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
pub struct GitLabMRSource {
    #[serde(default)]
    pub git_http_url: Option<String>,
    #[serde(default)]
    pub git_ssh_url: Option<String>,
}

#[derive(Deserialize)]
pub struct GitLabUser {
    #[serde(default)]
    pub username: Option<String>,
}

#[derive(Deserialize)]
pub struct GitLabCommit {
    pub id: String,
}

#[derive(Deserialize)]
pub struct GitLabReleasePayload {
    pub project: GitLabProject,
    pub commit: Option<GitLabCommit>,
    pub tag: Option<String>,
}

#[derive(Deserialize)]
pub struct GitHubReviewPayload {
    pub action: String,
    pub review: GitHubReview,
    pub pull_request: GitHubReviewPr,
    pub repository: GitHubRepository,
    #[serde(default)]
    pub sender: Option<GitHubUser>,
}

#[derive(Deserialize)]
pub struct GitHubReview {
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub user: Option<GitHubUser>,
    #[serde(default)]
    pub commit_id: Option<String>,
}

#[derive(Deserialize)]
pub struct GitHubReviewPr {
    #[serde(default)]
    pub number: Option<u64>,
}

#[derive(Deserialize)]
pub struct GiteaReviewPayload {
    pub action: String,
    pub review: GiteaReview,
    pub pull_request: GiteaReviewPr,
    pub repository: GiteaRepository,
    #[serde(default)]
    pub sender: Option<GiteaUser>,
}

#[derive(Deserialize)]
pub struct GiteaReview {
    #[serde(rename = "type", default)]
    pub review_type: Option<String>,
}

#[derive(Deserialize)]
pub struct GiteaReviewPr {
    #[serde(default)]
    pub number: Option<u64>,
}

pub struct ParsedPushEvent {
    pub commit_hash: Vec<u8>,
    pub repository_urls: Vec<String>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
    pub ref_name: String,
    pub is_tag: bool,
    pub default_branch: Option<String>,
}

pub enum PushOutcome {
    Build(ParsedPushEvent),
    Ignored,
}

pub struct ParsedMergeGroupEvent {
    pub commit_hash: Vec<u8>,
    pub repository_urls: Vec<String>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
    pub queue_branch: String,
    pub base_branch: String,
}

pub enum MergeGroupOutcome {
    Build(ParsedMergeGroupEvent),
    Ignored,
}

pub struct ParsedPullRequestEvent {
    pub commit_hash: Vec<u8>,
    pub repository_urls: Vec<String>,
    pub action: String,
    pub branch: Option<String>,
    pub base_branch: Option<String>,
    pub pr_number: Option<u64>,
    pub pr_author: Option<String>,
    /// `None` is set when the payload cannot decide. Callers must treat it as untrusted.
    pub is_fork: Option<bool>,
    pub head_repo_owner_login: Option<String>,
    pub unknown_author_requires_approval: bool,
    pub head_repo_clone_url: Option<RepositoryUrl>,
    pub title: Option<String>,
}

pub struct ParsedReleaseEvent {
    pub target: ReleaseTarget,
    pub repository_urls: Vec<String>,
    pub tag: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseTarget {
    Commit(Vec<u8>),
    Tag(String),
}

impl ReleaseTarget {
    fn of(commit_hash: Option<Vec<u8>>, tag: Option<&str>) -> Option<Self> {
        commit_hash
            .map(Self::Commit)
            .or_else(|| tag.map(|t| Self::Tag(t.to_owned())))
    }
}

pub struct ParsedPullRequestReviewEvent {
    pub approved: bool,
    pub reviewer: Option<String>,
    pub pr_number: Option<u64>,
    pub repository_full_name: Option<String>,
    pub approved_commit_source: Option<ApprovedCommitSource>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ApprovedCommitSource {
    CommitId(Vec<u8>),
    ReviewSenderUserIdOnly { sender_user_id: i64 },
}

pub struct PushCommit {
    pub hash: Vec<u8>,
    pub ref_name: String,
    pub is_tag: bool,
}

pub fn decode_push_commit(git_ref: &str, after: &str, git_host: &str) -> Option<PushCommit> {
    if after == "0000000000000000000000000000000000000000" {
        return None;
    }
    let (ref_name, is_tag) = if let Some(branch) = git_ref.strip_prefix("refs/heads/") {
        (branch.to_string(), false)
    } else {
        let tag = git_ref.strip_prefix("refs/tags/")?;
        (tag.to_string(), true)
    };
    match hex::decode(after) {
        Ok(hash) => Some(PushCommit {
            hash,
            ref_name,
            is_tag,
        }),
        Err(e) => {
            warn!(error = %e, sha = %after, git_host, "Push webhook: invalid commit SHA");
            None
        }
    }
}

pub(crate) fn parse_allowed_fork_repository_clone_url(
    url: &str,
    git_host: &str,
) -> Option<RepositoryUrl> {
    match url.parse() {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            warn!(error = %e, url, git_host, "ignore fork clone URL that is not an allowed repository URL");
            None
        }
    }
}

fn decode_sha_hex(s: &str, git_host: &str, context: &str) -> Option<Vec<u8>> {
    if s.len() != 40 {
        return None;
    }
    match hex::decode(s) {
        Ok(b) => Some(b),
        Err(e) => {
            warn!(error = %e, sha = %s, git_host, context, "invalid SHA");
            None
        }
    }
}

fn normalise_gitea_pr_action(action: String) -> String {
    match action.as_str() {
        "synchronized" => "synchronize".to_owned(),
        _ => action,
    }
}

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

impl ParsedPushEvent {
    pub fn from_github(body: &[u8]) -> Option<PushOutcome> {
        let payload: GitHubPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitHub push payload");
                return None;
            }
        };
        let head = payload.head_commit.as_ref();
        let sha = head_commit_id(head).unwrap_or(&payload.after);
        let Some(pc) = decode_push_commit(&payload.git_ref, sha, "github") else {
            return Some(PushOutcome::Ignored);
        };
        Some(PushOutcome::Build(Self {
            commit_hash: pc.hash,
            default_branch: payload.repository.default_branch,
            repository_urls: vec![payload.repository.clone_url, payload.repository.ssh_url],
            commit_message: head.and_then(commit_message),
            author_name: head
                .and_then(|c| c.author.as_ref())
                .and_then(|a| a.name.clone()),
            ref_name: pc.ref_name,
            is_tag: pc.is_tag,
        }))
    }

    pub fn from_gitea(body: &[u8]) -> Option<PushOutcome> {
        let payload: GiteaPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse Gitea/Forgejo push payload");
                return None;
            }
        };
        let head = payload.head_commit.as_ref();
        let sha = head_commit_id(head).unwrap_or(&payload.after);
        let Some(pc) = decode_push_commit(&payload.git_ref, sha, "gitea") else {
            return Some(PushOutcome::Ignored);
        };
        let mut urls = vec![payload.repository.clone_url];
        if let Some(ssh) = payload.repository.ssh_url {
            urls.push(ssh);
        }

        Some(PushOutcome::Build(Self {
            commit_hash: pc.hash,
            default_branch: payload.repository.default_branch,
            repository_urls: urls,
            commit_message: head.and_then(commit_message),
            author_name: head
                .and_then(|c| c.author.as_ref())
                .and_then(|a| a.name.clone()),
            ref_name: pc.ref_name,
            is_tag: pc.is_tag,
        }))
    }

    pub fn from_gitlab(body: &[u8]) -> Option<PushOutcome> {
        let payload: GitLabPushPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitLab push payload");
                return None;
            }
        };
        let sha = payload.checkout_sha.as_deref().unwrap_or(&payload.after);
        let Some(pc) = decode_push_commit(&payload.git_ref, sha, "gitlab") else {
            return Some(PushOutcome::Ignored);
        };
        let mut urls = vec![payload.project.http_url];
        if let Some(ssh) = payload.project.ssh_url {
            urls.push(ssh);
        }
        let default_branch = payload.project.default_branch;

        let head = payload
            .commits
            .iter()
            .find(|c| c.id.as_deref() == Some(sha))
            .or_else(|| payload.commits.last());
        Some(PushOutcome::Build(Self {
            commit_hash: pc.hash,
            default_branch,
            repository_urls: urls,
            commit_message: head.and_then(commit_message),
            author_name: head
                .and_then(|c| c.author.as_ref())
                .and_then(|a| a.name.clone()),
            ref_name: pc.ref_name,
            is_tag: pc.is_tag,
        }))
    }
}

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
        let head_repo_owner_login = head_full
            .as_deref()
            .and_then(|full| full.split_once('/'))
            .map(|(owner, _)| owner.to_owned());
        let pr_author = payload
            .pull_request
            .user
            .as_ref()
            .and_then(|u| u.login.clone());
        let mut repository_urls = Vec::with_capacity(4);
        let mut head_repo_clone_url = None;
        if let (Some(true), Some(repo)) = (is_fork, head_repo) {
            if let Some(url) = repo.clone_url.clone() {
                head_repo_clone_url = parse_allowed_fork_repository_clone_url(&url, "github");
                repository_urls.push(url);
            }
            if let Some(url) = repo.ssh_url.clone() {
                repository_urls.push(url);
            }
        }
        repository_urls.push(payload.repository.clone_url);
        repository_urls.push(payload.repository.ssh_url);
        Some(Self {
            commit_hash,
            repository_urls,
            action: payload.action,
            branch: Some(payload.pull_request.head.branch),
            base_branch: payload.pull_request.base.map(|base| base.branch),
            pr_number: payload.pull_request.number,
            pr_author,
            is_fork,
            head_repo_owner_login,
            unknown_author_requires_approval: false,
            head_repo_clone_url,
            title: payload.pull_request.title,
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
        let base_full = payload.repository.full_name.as_deref();
        let head_repo_owner_login = payload
            .pull_request
            .head
            .repo
            .as_ref()
            .and_then(|r| r.full_name.as_deref())
            .and_then(|full| full.split_once('/'))
            .map(|(owner, _)| owner.to_owned());
        let is_fork = payload.pull_request.is_fork(base_full);
        let fork_repo = payload.pull_request.fork_repo(base_full);
        let pr_author = payload
            .pull_request
            .user
            .as_ref()
            .and_then(|u| u.username.clone().or_else(|| u.login.clone()));
        let head_repo_clone_url = fork_repo
            .and_then(|r| r.clone_url.as_deref())
            .and_then(|url| parse_allowed_fork_repository_clone_url(url, "gitea"));
        let mut repository_urls: Vec<String> = fork_repo
            .into_iter()
            .flat_map(|r| [r.clone_url.clone(), r.ssh_url.clone()])
            .flatten()
            .collect();
        repository_urls.extend(gitea_repo_urls(&payload.repository));
        Some(Self {
            commit_hash,
            repository_urls,
            action: normalise_gitea_pr_action(payload.action),
            branch: payload.pull_request.head_branch(),
            base_branch: payload.pull_request.base.and_then(|base| base.git_ref),
            pr_number: payload.pull_request.number,
            pr_author,
            is_fork,
            head_repo_owner_login,
            unknown_author_requires_approval: false,
            head_repo_clone_url,
            title: payload.pull_request.title,
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
        let mut repository_urls = Vec::with_capacity(4);
        let mut head_repo_clone_url = None;
        if let (Some(true), Some(src)) = (is_fork, payload.object_attributes.source.as_ref()) {
            if let Some(url) = src.git_http_url.clone() {
                head_repo_clone_url = parse_allowed_fork_repository_clone_url(&url, "gitlab");
                repository_urls.push(url);
            }
            if let Some(url) = src.git_ssh_url.clone() {
                repository_urls.push(url);
            }
        }
        repository_urls.extend(gitlab_project_urls(&payload.project));
        Some(Self {
            commit_hash,
            repository_urls,
            action,
            branch: Some(payload.object_attributes.source_branch),
            base_branch: payload.object_attributes.target_branch,
            pr_number: payload.object_attributes.iid,
            pr_author: None,
            is_fork,
            head_repo_owner_login: None,
            unknown_author_requires_approval: true,
            head_repo_clone_url,
            title: payload.object_attributes.title,
        })
    }
}

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
        );
        let tag = payload.release.tag_name;
        Some(Self {
            target: ReleaseTarget::of(commit_hash, Some(&tag))?,
            repository_urls: vec![payload.repository.clone_url, payload.repository.ssh_url],
            tag: Some(tag),
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
            });
        let tag = payload.release.tag_name;
        Some(Self {
            target: ReleaseTarget::of(commit_hash, Some(&tag))?,
            repository_urls: gitea_repo_urls(&payload.repository),
            tag: Some(tag),
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
            .and_then(|c| decode_sha_hex(&c.id, "gitlab", "commit.id"));
        Some(Self {
            target: ReleaseTarget::of(commit_hash, payload.tag.as_deref())?,
            repository_urls: gitlab_project_urls(&payload.project),
            tag: payload.tag,
        })
    }
}

impl ParsedMergeGroupEvent {
    pub fn from_github(body: &[u8]) -> Option<MergeGroupOutcome> {
        let payload: GitHubMergeGroupPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitHub merge_group payload");
                return None;
            }
        };
        if payload.action != "checks_requested" {
            return Some(MergeGroupOutcome::Ignored);
        }

        let group = payload.merge_group;
        let commit_hash = decode_sha_hex(&group.head_sha, "github", "merge_group.head_sha")?;
        let head = group.head_commit.as_ref();
        Some(MergeGroupOutcome::Build(Self {
            commit_hash,
            repository_urls: vec![payload.repository.clone_url, payload.repository.ssh_url],
            commit_message: head.and_then(commit_message),
            author_name: head
                .and_then(|c| c.author.as_ref())
                .and_then(|a| a.name.clone()),
            queue_branch: group.head_ref.strip_prefix("refs/heads/")?.to_owned(),
            base_branch: group.base_ref.strip_prefix("refs/heads/")?.to_owned(),
        }))
    }
}

impl ParsedPullRequestReviewEvent {
    pub fn from_github(body: &[u8]) -> Option<Self> {
        let payload: GitHubReviewPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse GitHub pull_request_review payload");
                return None;
            }
        };
        let approved = payload.action == "submitted"
            && payload
                .review
                .state
                .as_deref()
                .is_some_and(|s| s.eq_ignore_ascii_case("approved"));
        let reviewer = payload
            .review
            .user
            .and_then(|u| u.login)
            .or_else(|| payload.sender.and_then(|s| s.login));
        let approved_commit_source = payload
            .review
            .commit_id
            .as_deref()
            .and_then(|sha| decode_sha_hex(sha, "github", "review.commit_id"))
            .map(ApprovedCommitSource::CommitId);
        Some(Self {
            approved,
            reviewer,
            pr_number: payload.pull_request.number,
            repository_full_name: payload.repository.full_name,
            approved_commit_source,
        })
    }

    pub fn from_gitea(body: &[u8]) -> Option<Self> {
        let payload: GiteaReviewPayload = match serde_json::from_slice(body) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "Failed to parse Gitea/Forgejo pull_request_review payload");
                return None;
            }
        };
        let approved = payload.action == "reviewed"
            && payload.review.review_type.as_deref() == Some("pull_request_review_approved");
        let (sender_user_id, reviewer) = payload
            .sender
            .map_or((None, None), |s| (s.id, s.username.or(s.login)));
        let approved_commit_source = sender_user_id
            .filter(|id| *id > 0)
            .map(|sender_user_id| ApprovedCommitSource::ReviewSenderUserIdOnly { sender_user_id });
        Some(Self {
            approved,
            reviewer,
            pr_number: payload.pull_request.number,
            repository_full_name: payload.repository.full_name,
            approved_commit_source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_SHA: &str = "abcdef0123456789abcdef0123456789abcdef01";
    const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

    fn merge_group_payload(action: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "action": action,
            "merge_group": {
                "head_sha": VALID_SHA,
                "head_ref": "refs/heads/gh-readonly-queue/main/pr-7-0123",
                "base_sha": ZERO_SHA,
                "base_ref": "refs/heads/main",
                "head_commit": {
                    "id": VALID_SHA,
                    "message": "Merge pull request #7\n\nbody",
                    "author": { "name": "octocat" },
                },
            },
            "repository": {
                "clone_url": "https://github.com/acme/widgets.git",
                "ssh_url": "git@github.com:acme/widgets.git",
            },
        }))
        .unwrap()
    }

    #[test]
    fn a_requested_merge_group_builds_its_head_on_the_queue_branch() {
        let Some(MergeGroupOutcome::Build(ev)) =
            ParsedMergeGroupEvent::from_github(&merge_group_payload("checks_requested"))
        else {
            panic!("expected a buildable merge group");
        };
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
        assert_eq!(ev.queue_branch, "gh-readonly-queue/main/pr-7-0123");
        assert_eq!(ev.base_branch, "main");
        assert_eq!(
            ev.commit_message.as_deref(),
            Some("Merge pull request #7\n\nbody")
        );
        assert_eq!(ev.author_name.as_deref(), Some("octocat"));
    }

    #[test]
    fn a_destroyed_merge_group_builds_nothing() {
        assert!(matches!(
            ParsedMergeGroupEvent::from_github(&merge_group_payload("destroyed")),
            Some(MergeGroupOutcome::Ignored)
        ));
    }

    #[track_caller]
    fn build(outcome: Option<PushOutcome>) -> ParsedPushEvent {
        match outcome {
            Some(PushOutcome::Build(ev)) => ev,
            _ => panic!("expected a buildable push"),
        }
    }

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

    #[test]
    fn parse_github_pr_opened_event() {
        let body = format!(
            r#"{{
                "action": "opened",
                "pull_request": {{
                    "head": {{
                        "sha": "{VALID_SHA}",
                        "ref": "feature-x"
                    }},
                    "title": "Add the widget"
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
        assert_eq!(ev.title, Some("Add the widget".to_string()));
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
        assert_eq!(
            ev.head_repo_owner_login.as_deref(),
            Some("external-contrib")
        );
        assert_eq!(ev.is_fork, Some(true));
        assert_eq!(
            (ev.branch.as_deref(), ev.base_branch.as_deref()),
            (Some("patch-1"), Some("main"))
        );
    }

    #[test]
    fn force_push_pull_request_event_reports_author_not_sender() {
        let body = format!(
            r#"{{
                "action": "synchronize",
                "sender": {{ "login": "maintainer" }},
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
        assert_eq!(ev.is_fork, Some(true));
        assert_eq!(ev.pr_author.as_deref(), Some("external-contrib"));
        assert_eq!(
            ev.head_repo_owner_login.as_deref(),
            Some("external-contrib")
        );
    }

    #[test]
    fn gitea_maintainer_pull_request_event_reports_author_username() {
        let body = format!(
            r#"{{
                "action": "synchronize",
                "sender": {{ "username": "maintainer" }},
                "pull_request": {{
                    "user": {{ "username": "external-contrib" }},
                    "head": {{ "sha": "{VALID_SHA}", "ref": "feat" }}
                }},
                "repository": {{ "clone_url": "https://gitea.example.com/org/repo.git" }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_gitea(body.as_bytes()).unwrap();
        assert_eq!(ev.pr_author.as_deref(), Some("external-contrib"));
    }

    #[test]
    fn gitlab_merge_request_event_author_stays_unknown() {
        let body = format!(
            r#"{{
                "object_attributes": {{
                    "action": "update",
                    "iid": 11,
                    "source_branch": "feat",
                    "last_commit": {{ "id": "{VALID_SHA}" }}
                }},
                "project": {{ "http_url": "https://gitlab.example.com/group/repo.git" }},
                "user": {{ "username": "maintainer" }}
            }}"#
        );
        let ev = ParsedPullRequestEvent::from_gitlab(body.as_bytes()).unwrap();
        assert_eq!(ev.pr_author, None);
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

    fn parse_fork_pull_request_webhook_bodies_of_github_gitea_and_gitlab_with_clone_url(
        clone_url: &str,
    ) -> [ParsedPullRequestEvent; 3] {
        let fork = format!(
            r#""full_name": "external/repo", "clone_url": "{clone_url}", "ssh_url": "{clone_url}""#
        );
        let github = format!(
            r#"{{
                "action": "opened",
                "pull_request": {{
                    "head": {{ "sha": "{VALID_SHA}", "ref": "patch-1", "repo": {{ {fork} }} }},
                    "base": {{ "sha": "{VALID_SHA}", "ref": "main", "repo": {{ "full_name": "org/repo" }} }}
                }},
                "repository": {{
                    "clone_url": "https://github.com/org/repo.git",
                    "ssh_url": "git@github.com:org/repo.git",
                    "full_name": "org/repo"
                }}
            }}"#
        );
        let gitea = format!(
            r#"{{
                "action": "opened",
                "pull_request": {{
                    "head": {{ "sha": "{VALID_SHA}", "ref": "patch-1", "repo": {{ {fork} }} }},
                    "base": {{ "sha": "{VALID_SHA}", "ref": "main", "repo": {{ "full_name": "org/repo" }} }}
                }},
                "repository": {{
                    "clone_url": "https://gitea.example.com/org/repo.git",
                    "ssh_url": "git@gitea.example.com:org/repo.git",
                    "full_name": "org/repo"
                }}
            }}"#
        );
        let gitlab = format!(
            r#"{{
                "object_attributes": {{
                    "action": "open",
                    "source_branch": "patch-1",
                    "source_project_id": 99,
                    "target_project_id": 1,
                    "source": {{ "git_http_url": "{clone_url}", "git_ssh_url": "{clone_url}" }},
                    "last_commit": {{ "id": "{VALID_SHA}" }}
                }},
                "project": {{
                    "http_url": "https://gitlab.example.com/group/repo.git",
                    "ssh_url": "git@gitlab.example.com:group/repo.git",
                    "path_with_namespace": "group/repo"
                }}
            }}"#
        );
        [
            ParsedPullRequestEvent::from_github(github.as_bytes()).unwrap(),
            ParsedPullRequestEvent::from_gitea(gitea.as_bytes()).unwrap(),
            ParsedPullRequestEvent::from_gitlab(gitlab.as_bytes()).unwrap(),
        ]
    }

    #[test]
    fn fork_pull_request_webhook_gives_no_clone_url_for_fork_clone_url_on_server_disk_and_keeps_https_for_all_git_hosts()
     {
        for ev in parse_fork_pull_request_webhook_bodies_of_github_gitea_and_gitlab_with_clone_url(
            "file:///srv/repo",
        ) {
            assert_eq!(ev.is_fork, Some(true));
            assert_eq!(ev.head_repo_clone_url, None);
        }
        for ev in parse_fork_pull_request_webhook_bodies_of_github_gitea_and_gitlab_with_clone_url(
            "https://example.com/external/repo.git",
        ) {
            assert_eq!(
                ev.head_repo_clone_url
                    .as_ref()
                    .map(RepositoryUrl::git_connection_url),
                Some("https://example.com/external/repo.git")
            );
        }
    }

    #[test]
    fn gitlab_mr_different_projects_marks_is_fork() {
        let body = format!(
            r#"{{
                "object_attributes": {{
                    "action": "open",
                    "iid": 11,
                    "source_branch": "feat",
                    "target_branch": "main",
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
        assert_eq!(ev.pr_author, None);
        assert_eq!(ev.is_fork, Some(true));
        assert_eq!(ev.base_branch.as_deref(), Some("main"));
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
    fn parse_gitea_pr_falls_back_to_label_for_branch() {
        let body = format!(
            r#"{{
                "action": "synchronize",
                "pull_request": {{
                    "head": {{
                        "sha": "{VALID_SHA}",
                        "label": "fallback-branch"
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

    fn gitea_same_repo_pr(forgejo_flow: Option<u8>, head_ref: &str, label: &str) -> Vec<u8> {
        let flow = forgejo_flow.map_or_else(String::new, |f| format!(r#""flow": {f},"#));
        format!(
            r#"{{
                "action": "opened",
                "pull_request": {{
                    {flow}
                    "head": {{
                        "sha": "{VALID_SHA}",
                        "ref": "{head_ref}",
                        "label": "{label}",
                        "repo": {{
                            "full_name": "org/repo",
                            "clone_url": "https://gitea.example.com/org/repo.git"
                        }}
                    }},
                    "base": {{
                        "sha": "{VALID_SHA}",
                        "ref": "main",
                        "label": "main",
                        "repo": {{ "full_name": "org/repo" }}
                    }}
                }},
                "repository": {{
                    "full_name": "org/repo",
                    "clone_url": "https://gitea.example.com/org/repo.git",
                    "ssh_url": "git@gitea.example.com:org/repo.git"
                }}
            }}"#
        )
        .into_bytes()
    }

    #[test]
    fn gitea_agit_pr_is_untrusted_and_built_from_the_base_repo() {
        let body = gitea_same_repo_pr(None, "refs/pull/7/head", "");
        let ev = ParsedPullRequestEvent::from_gitea(&body).unwrap();
        assert_eq!(ev.is_fork, None);
        assert_eq!(ev.head_repo_clone_url, None);
        assert_eq!(ev.branch.as_deref(), Some("refs/pull/7/head"));
        assert_eq!(
            ev.repository_urls,
            [
                "https://gitea.example.com/org/repo.git",
                "git@gitea.example.com:org/repo.git"
            ]
        );
    }

    #[test]
    fn forgejo_agit_pr_is_untrusted_and_named_by_its_topic() {
        let body = gitea_same_repo_pr(Some(1), "refs/pull/7/head", "alice/topic");
        let ev = ParsedPullRequestEvent::from_gitea(&body).unwrap();
        assert_eq!(ev.is_fork, None);
        assert_eq!(ev.branch.as_deref(), Some("alice/topic"));
    }

    #[test]
    fn gitea_same_repo_pr_with_a_deleted_branch_stays_trusted() {
        let body = gitea_same_repo_pr(Some(0), "refs/pull/3/head", "feature");
        let ev = ParsedPullRequestEvent::from_gitea(&body).unwrap();
        assert_eq!(ev.is_fork, Some(false));
        assert_eq!(ev.branch.as_deref(), Some("feature"));
    }

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

    #[test]
    fn a_gitea_push_to_a_pull_request_matches_the_default_actions() {
        assert_eq!(
            normalise_gitea_pr_action("synchronized".to_owned()),
            "synchronize"
        );
        assert_eq!(normalise_gitea_pr_action("opened".to_owned()), "opened");
    }

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
        assert_eq!(
            ev.target,
            ReleaseTarget::Commit(hex::decode(VALID_SHA).unwrap())
        );
    }

    #[test]
    fn a_github_release_targeting_a_branch_resolves_through_its_tag() {
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
        let ev = ParsedReleaseEvent::from_github(body.as_bytes()).unwrap();
        assert_eq!(ev.target, ReleaseTarget::Tag("v1.2.3".to_owned()));
        assert_eq!(ev.tag, Some("v1.2.3".to_owned()));
    }

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
        assert_eq!(
            ev.target,
            ReleaseTarget::Commit(hex::decode(VALID_SHA).unwrap())
        );
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
        assert_eq!(
            ev.target,
            ReleaseTarget::Commit(hex::decode(VALID_SHA).unwrap())
        );
    }

    #[test]
    fn a_gitea_release_without_a_sha_resolves_through_its_tag() {
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
        let ev = ParsedReleaseEvent::from_gitea(body.as_bytes()).unwrap();
        assert_eq!(ev.target, ReleaseTarget::Tag("v2.2.0".to_owned()));
    }

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
        assert_eq!(
            ev.target,
            ReleaseTarget::Commit(hex::decode(VALID_SHA).unwrap())
        );
    }

    #[test]
    fn a_gitlab_release_without_a_commit_resolves_through_its_tag() {
        let body = r#"{
            "action": "create",
            "project": {
                "http_url": "https://gitlab.example.com/org/repo.git"
            },
            "tag": "v3.1.0"
        }"#;
        let ev = ParsedReleaseEvent::from_gitlab(body.as_bytes()).unwrap();
        assert_eq!(ev.target, ReleaseTarget::Tag("v3.1.0".to_owned()));
    }

    #[test]
    fn a_gitlab_release_without_a_commit_or_tag_returns_none() {
        let body = r#"{
            "action": "create",
            "project": {
                "http_url": "https://gitlab.example.com/org/repo.git"
            }
        }"#;
        assert!(ParsedReleaseEvent::from_gitlab(body.as_bytes()).is_none());
    }

    #[test]
    fn github_review_approved_by_maintainer() {
        let body = r#"{
            "action": "submitted",
            "review": {
                "state": "approved",
                "user": { "login": "maintainer" },
                "commit_id": "abcdef0123456789abcdef0123456789abcdef01"
            },
            "pull_request": { "number": 42, "head": { "sha": "1111111111111111111111111111111111111111" } },
            "repository": { "clone_url": "x", "ssh_url": "y", "full_name": "org/repo" }
        }"#;
        let ev = ParsedPullRequestReviewEvent::from_github(body.as_bytes()).unwrap();
        assert!(ev.approved);
        assert_eq!(
            ev.approved_commit_source,
            Some(ApprovedCommitSource::CommitId(
                hex::decode(VALID_SHA).unwrap()
            ))
        );
        assert_eq!(ev.reviewer.as_deref(), Some("maintainer"));
        assert_eq!(ev.pr_number, Some(42));
        assert_eq!(ev.repository_full_name.as_deref(), Some("org/repo"));
    }

    #[test]
    fn github_review_without_commit_id_has_no_commit_hash() {
        let body = r#"{
            "action": "submitted",
            "review": { "state": "approved", "user": { "login": "maintainer" } },
            "pull_request": { "number": 42 },
            "repository": { "clone_url": "x", "ssh_url": "y", "full_name": "org/repo" }
        }"#;
        let ev = ParsedPullRequestReviewEvent::from_github(body.as_bytes()).unwrap();
        assert_eq!(ev.approved_commit_source, None);
    }

    #[test]
    fn github_review_changes_requested_is_not_approved() {
        let body = r#"{
            "action": "submitted",
            "review": { "state": "changes_requested", "user": { "login": "maintainer" } },
            "pull_request": { "number": 42 },
            "repository": { "clone_url": "x", "ssh_url": "y", "full_name": "org/repo" }
        }"#;
        let ev = ParsedPullRequestReviewEvent::from_github(body.as_bytes()).unwrap();
        assert!(!ev.approved);
    }

    #[test]
    fn github_review_dismissed_is_not_approved() {
        let body = r#"{
            "action": "dismissed",
            "review": { "state": "approved", "user": { "login": "maintainer" } },
            "pull_request": { "number": 42 },
            "repository": { "clone_url": "x", "ssh_url": "y", "full_name": "org/repo" }
        }"#;
        let ev = ParsedPullRequestReviewEvent::from_github(body.as_bytes()).unwrap();
        assert!(!ev.approved, "only freshly submitted approvals count");
    }

    #[test]
    fn gitea_approved_review_with_empty_commit_id_and_head_sha_uses_sender_user_id_only() {
        let body = r#"{
            "action": "reviewed",
            "review": { "type": "pull_request_review_approved", "content": "" },
            "commit_id": "",
            "pull_request": {
                "number": 7,
                "head": { "sha": "abcdef0123456789abcdef0123456789abcdef01", "ref": "feature", "label": "feature" }
            },
            "repository": { "clone_url": "x", "full_name": "org/repo" },
            "sender": { "id": 42, "username": "maintainer" }
        }"#;
        let ev = ParsedPullRequestReviewEvent::from_gitea(body.as_bytes()).unwrap();
        assert!(ev.approved);
        assert_eq!(ev.reviewer.as_deref(), Some("maintainer"));
        assert_eq!(ev.pr_number, Some(7));
        assert_eq!(ev.repository_full_name.as_deref(), Some("org/repo"));
        assert_eq!(
            ev.approved_commit_source,
            Some(ApprovedCommitSource::ReviewSenderUserIdOnly { sender_user_id: 42 })
        );
    }

    #[test]
    fn gitea_approved_review_with_sender_id_zero_or_missing_has_no_reviewed_commit_source() {
        for sender in [
            r#"{ "id": 0, "username": "maintainer" }"#,
            r#"{ "id": -1, "username": "maintainer" }"#,
            r#"{ "username": "maintainer" }"#,
        ] {
            let body = format!(
                r#"{{
                    "action": "reviewed",
                    "review": {{ "type": "pull_request_review_approved" }},
                    "commit_id": "abcdef0123456789abcdef0123456789abcdef01",
                    "pull_request": {{ "number": 7 }},
                    "repository": {{ "clone_url": "x", "full_name": "org/repo" }},
                    "sender": {sender}
                }}"#
            );
            let ev = ParsedPullRequestReviewEvent::from_gitea(body.as_bytes()).unwrap();
            assert!(ev.approved);
            assert_eq!(ev.approved_commit_source, None, "{sender}");
        }
    }

    #[test]
    fn gitea_review_rejected_is_not_approved() {
        let body = r#"{
            "action": "reviewed",
            "review": { "type": "pull_request_review_rejected" },
            "pull_request": { "number": 7 },
            "repository": { "clone_url": "x", "full_name": "org/repo" },
            "sender": { "username": "maintainer" }
        }"#;
        let ev = ParsedPullRequestReviewEvent::from_gitea(body.as_bytes()).unwrap();
        assert!(!ev.approved);
    }

    #[test]
    fn github_push_keeps_the_whole_commit_message_and_author() {
        let body = format!(
            r#"{{
                "ref": "refs/heads/main",
                "after": "{VALID_SHA}",
                "repository": {{ "clone_url": "https://github.com/org/repo.git", "ssh_url": "git@github.com:org/repo.git" }},
                "head_commit": {{ "id": "{VALID_SHA}", "message": "feat: add thing\n\nlong body", "author": {{ "name": "Octo Cat" }} }}
            }}"#
        );
        let ev = build(ParsedPushEvent::from_github(body.as_bytes()));
        assert_eq!(
            ev.commit_message.as_deref(),
            Some("feat: add thing\n\nlong body")
        );
        assert_eq!(ev.author_name.as_deref(), Some("Octo Cat"));
    }

    #[test]
    fn github_push_without_head_commit_has_no_message() {
        let body = format!(
            r#"{{
                "ref": "refs/heads/main",
                "after": "{VALID_SHA}",
                "repository": {{ "clone_url": "https://github.com/org/repo.git", "ssh_url": "git@github.com:org/repo.git" }}
            }}"#
        );
        let ev = build(ParsedPushEvent::from_github(body.as_bytes()));
        assert_eq!(ev.commit_message, None);
        assert_eq!(ev.author_name, None);
    }

    #[test]
    fn gitlab_push_picks_commit_matching_after() {
        let body = format!(
            r#"{{
                "ref": "refs/heads/main",
                "after": "{VALID_SHA}",
                "project": {{ "http_url": "https://gitlab.example.com/org/repo.git" }},
                "commits": [
                    {{ "id": "ffffffffffffffffffffffffffffffffffffffff", "message": "old" }},
                    {{ "id": "{VALID_SHA}", "message": "fix: the bug\nwith detail", "author": {{ "name": "Dev" }} }}
                ]
            }}"#
        );
        let ev = build(ParsedPushEvent::from_gitlab(body.as_bytes()));
        assert_eq!(
            ev.commit_message.as_deref(),
            Some("fix: the bug\nwith detail")
        );
        assert_eq!(ev.author_name.as_deref(), Some("Dev"));
    }

    const TAG_OBJECT_SHA: &str = "ef0fe4434c157e48287c7c4dc4424ad33334695b";

    #[test]
    fn github_annotated_tag_push_builds_the_tagged_commit() {
        let body = format!(
            r#"{{
                "ref": "refs/tags/v1.4.0",
                "after": "{TAG_OBJECT_SHA}",
                "repository": {{ "clone_url": "https://github.com/org/repo.git", "ssh_url": "git@github.com:org/repo.git" }},
                "head_commit": {{ "id": "{VALID_SHA}", "message": "version bump" }}
            }}"#
        );
        let ev = build(ParsedPushEvent::from_github(body.as_bytes()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
        assert!(ev.is_tag);
    }

    #[test]
    fn gitea_annotated_tag_push_builds_the_tagged_commit() {
        let body = format!(
            r#"{{
                "ref": "refs/tags/v1.4.0",
                "after": "{TAG_OBJECT_SHA}",
                "repository": {{ "clone_url": "https://gitea.example.com/org/repo.git" }},
                "head_commit": {{ "id": "{VALID_SHA}", "message": "version bump" }}
            }}"#
        );
        let ev = build(ParsedPushEvent::from_gitea(body.as_bytes()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
    }

    #[test]
    fn gitlab_annotated_tag_push_builds_the_checkout_sha() {
        let body = format!(
            r#"{{
                "ref": "refs/tags/v1.4.0",
                "after": "{TAG_OBJECT_SHA}",
                "checkout_sha": "{VALID_SHA}",
                "project": {{ "http_url": "https://gitlab.example.com/org/repo.git" }},
                "commits": []
            }}"#
        );
        let ev = build(ParsedPushEvent::from_gitlab(body.as_bytes()));
        assert_eq!(ev.commit_hash, hex::decode(VALID_SHA).unwrap());
        assert!(ev.is_tag);
    }

    #[test]
    fn gitlab_tag_deletion_is_ignored() {
        let body = format!(
            r#"{{
                "ref": "refs/tags/v1.4.0",
                "after": "{ZERO_SHA}",
                "checkout_sha": null,
                "project": {{ "http_url": "https://gitlab.example.com/org/repo.git" }},
                "commits": []
            }}"#
        );
        assert!(matches!(
            ParsedPushEvent::from_gitlab(body.as_bytes()),
            Some(PushOutcome::Ignored)
        ));
    }

    #[test]
    fn gitea_test_webhook_zero_sha_is_ignored_not_malformed() {
        let body = format!(
            r#"{{
                "ref": "refs/heads/master",
                "before": "{ZERO_SHA}",
                "after": "{ZERO_SHA}",
                "commits": [
                    {{ "id": "{ZERO_SHA}", "message": "This is a fake commit", "verification": null, "added": null, "removed": null, "modified": null }}
                ],
                "head_commit": {{ "id": "{ZERO_SHA}", "message": "This is a fake commit", "verification": null }},
                "repository": {{ "clone_url": "https://gitea.example.com/user/repo.git", "ssh_url": "git@gitea.example.com:user/repo.git" }}
            }}"#
        );
        assert!(matches!(
            ParsedPushEvent::from_gitea(body.as_bytes()),
            Some(PushOutcome::Ignored)
        ));
    }

    #[test]
    fn push_with_unparseable_json_is_malformed() {
        assert!(ParsedPushEvent::from_gitea(b"not json").is_none());
        assert!(ParsedPushEvent::from_github(b"{}").is_none());
    }

    #[test]
    fn github_branch_deletion_zero_sha_is_ignored() {
        let body = format!(
            r#"{{
                "ref": "refs/heads/feature",
                "after": "{ZERO_SHA}",
                "repository": {{ "clone_url": "https://github.com/org/repo.git", "ssh_url": "git@github.com:org/repo.git" }}
            }}"#
        );
        assert!(matches!(
            ParsedPushEvent::from_github(body.as_bytes()),
            Some(PushOutcome::Ignored)
        ));
    }

    #[test]
    fn push_carries_the_default_branch_of_each_forge() {
        let github = format!(
            r#"{{
                "ref": "refs/heads/feature",
                "after": "{VALID_SHA}",
                "repository": {{ "clone_url": "https://github.com/org/repo.git", "ssh_url": "git@github.com:org/repo.git", "default_branch": "main" }}
            }}"#
        );
        let gitea = format!(
            r#"{{
                "ref": "refs/heads/feature",
                "after": "{VALID_SHA}",
                "repository": {{ "clone_url": "https://gitea.example.com/org/repo.git", "default_branch": "trunk" }}
            }}"#
        );
        let gitlab = format!(
            r#"{{
                "ref": "refs/heads/feature",
                "after": "{VALID_SHA}",
                "project": {{ "http_url": "https://gitlab.example.com/org/repo.git", "default_branch": "master" }},
                "commits": []
            }}"#
        );
        let branch = |ev: ParsedPushEvent| ev.default_branch;
        assert_eq!(
            branch(build(ParsedPushEvent::from_github(github.as_bytes()))).as_deref(),
            Some("main")
        );
        assert_eq!(
            branch(build(ParsedPushEvent::from_gitea(gitea.as_bytes()))).as_deref(),
            Some("trunk")
        );
        assert_eq!(
            branch(build(ParsedPushEvent::from_gitlab(gitlab.as_bytes()))).as_deref(),
            Some("master")
        );
    }
}
