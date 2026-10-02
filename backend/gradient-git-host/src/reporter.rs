/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::pr::{BranchCommit, CommitIdent, PrRef};
use crate::registry::GitHostRegistry;
use anyhow::{Context, Result};
use async_trait::async_trait;
use gradient_types::GitHostType;
use gradient_util::http_validation::{WebhookUrlError, validate_webhook_url};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::warn;

fn author_or_bot(resolved: Result<CommitIdent>, base_url: &str) -> CommitIdent {
    resolved.unwrap_or_else(|e| {
        warn!(error = %e, "resolving Git host commit author; using Gradient bot default");
        CommitIdent::gradient_bot(base_url)
    })
}

fn validate_safe_outbound_url(url: &str) -> Result<(), WebhookUrlError> {
    validate_webhook_url(url).map(|_| ())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiStatus {
    Pending,
    Running,
    Success,
    Failure,
    Error,
    ActionRequired,
}

#[derive(Debug, Clone, Serialize)]
pub struct RequestedAction {
    pub identifier: String,
    pub label: String,
    pub description: String,
}

pub const APPROVAL_ACTION_ID: &str = "approve-and-run";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReactionKind {
    Eyes,
    ThumbsUp,
    ThumbsDown,
    Confused,
}

impl ReactionKind {
    pub const fn github_content(self) -> &'static str {
        match self {
            ReactionKind::Eyes => "eyes",
            ReactionKind::ThumbsUp => "+1",
            ReactionKind::ThumbsDown => "-1",
            ReactionKind::Confused => "confused",
        }
    }

    pub const fn gitlab_name(self) -> &'static str {
        match self {
            ReactionKind::Eyes => "eyes",
            ReactionKind::ThumbsUp => "thumbsup",
            ReactionKind::ThumbsDown => "thumbsdown",
            ReactionKind::Confused => "confused",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionTarget {
    pub owner: String,
    pub repo: String,
    pub pr_number: u64,
    pub comment_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PullRequestSnapshot {
    pub head_sha: String,
    pub head_branch: String,
    pub head_clone_url: Option<String>,
    pub is_fork: bool,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub struct CiReport {
    pub owner: String,
    pub repo: String,
    pub sha: String,
    pub context: String,
    pub status: CiStatus,
    pub description: Option<String>,
    pub details_url: Option<String>,
    pub existing_check_id: Option<i64>,
    pub requested_actions: Vec<RequestedAction>,
}

#[async_trait]
pub trait CiReporter: Send + Sync + std::fmt::Debug + 'static {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>>;

    /// The default must fail closed. Untrusted PRs are then always parked for approval on Git hosts
    /// without a permission probe.
    async fn is_repo_writer(&self, _owner: &str, _repo: &str, _username: &str) -> Result<bool> {
        Ok(false)
    }

    async fn post_pr_comment(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _body: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn get_pull_request(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        Ok(None)
    }

    async fn add_reaction(&self, _target: &ReactionTarget, _kind: ReactionKind) -> Result<()> {
        Ok(())
    }

    async fn upsert_branch(
        &self,
        _owner: &str,
        _repo: &str,
        _branch: &str,
        _base: &str,
        _commit: &BranchCommit,
    ) -> Result<String> {
        anyhow::bail!("this reporter does not support opening pull requests")
    }

    async fn open_or_update_pr(
        &self,
        _owner: &str,
        _repo: &str,
        _head: &str,
        _base: &str,
        _title: &str,
        _body: &str,
    ) -> Result<PrRef> {
        anyhow::bail!("this reporter does not support opening pull requests")
    }

    async fn default_branch(&self, _owner: &str, _repo: &str) -> Result<String> {
        anyhow::bail!("this reporter does not support opening pull requests")
    }

    async fn approve_pull_request(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _body: &str,
    ) -> Result<()> {
        Ok(())
    }

    async fn verify(&self, owner: &str, repo: &str) -> Result<()> {
        self.default_branch(owner, repo).await.map(|_| ())
    }
}

#[derive(Debug)]
pub struct NoopCiReporter;

#[async_trait]
impl CiReporter for NoopCiReporter {
    async fn report(&self, _report: &CiReport) -> Result<Option<i64>> {
        Ok(None)
    }
}

#[derive(Debug)]
pub struct GiteaReporter {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

fn gitea_comment_url(base_url: &str, owner: &str, repo: &str, pr_number: u64) -> String {
    format!(
        "{}/api/v1/repos/{}/{}/issues/{}/comments",
        base_url, owner, repo, pr_number
    )
}

#[derive(Debug, Serialize)]
struct GitHostCommentPayload<'a> {
    body: &'a str,
}

impl GiteaReporter {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self> {
        let raw = base_url.into();
        validate_safe_outbound_url(&raw)
            .map_err(|e| anyhow::anyhow!("Rejected Gitea base_url: {}", e))?;
        Ok(Self {
            base_url: raw.trim_end_matches('/').to_string(),
            token: token.into(),
            client,
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum GiteaState {
    Pending,
    Success,
    Error,
    Failure,
}

impl From<&CiStatus> for GiteaState {
    fn from(s: &CiStatus) -> Self {
        match s {
            CiStatus::Pending | CiStatus::Running | CiStatus::ActionRequired => GiteaState::Pending,
            CiStatus::Success => GiteaState::Success,
            CiStatus::Failure => GiteaState::Failure,
            CiStatus::Error => GiteaState::Error,
        }
    }
}

#[derive(Debug, Serialize)]
struct GiteaStatusPayload<'a> {
    state: GiteaState,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    context: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<&'a str>,
}

#[async_trait]
impl CiReporter for GiteaReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/statuses/{}",
            self.base_url, report.owner, report.repo, report.sha
        );

        let payload = GiteaStatusPayload {
            state: GiteaState::from(&report.status),
            description: report.description.as_deref(),
            context: &report.context,
            target_url: report.details_url.as_deref(),
        };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send Gitea status request")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(
                gitea_url = %url,
                http_status = %status,
                body = %body,
                "Gitea CI status report failed"
            );
            anyhow::bail!("Gitea returned {}: {}", status, body);
        }

        Ok(None)
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/collaborators/{}/permission",
            self.base_url, owner, repo, username
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .context("Failed to query Gitea collaborator permission")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gitea permission query returned {}: {}", status, body);
        }
        #[derive(Deserialize)]
        struct PermissionResponse {
            permission: String,
        }
        let parsed: PermissionResponse = resp
            .json()
            .await
            .context("Failed to parse Gitea permission response")?;
        Ok(matches!(
            parsed.permission.as_str(),
            "admin" | "owner" | "write"
        ))
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        let url = gitea_comment_url(&self.base_url, owner, repo, pr_number);
        let payload = GitHostCommentPayload { body };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send Gitea comment request")?;

        let status = resp.status();
        if !status.is_success() {
            let resp_body = resp.text().await.unwrap_or_default();
            warn!(
                gitea_url = %url,
                http_status = %status,
                body = %resp_body,
                "Gitea PR comment post failed"
            );
            anyhow::bail!("Gitea returned {}: {}", status, resp_body);
        }
        Ok(())
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/issues/comments/{}/reactions",
            self.base_url, target.owner, target.repo, target.comment_id
        );
        #[derive(Serialize)]
        struct Body<'a> {
            content: &'a str,
        }
        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("token {}", self.token))
            .header("Content-Type", "application/json")
            .json(&Body {
                content: kind.github_content(),
            })
            .send()
            .await
            .context("Failed to send Gitea comment reaction")?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(
                gitea_url = %url,
                http_status = %status,
                body = %body,
                "Gitea comment reaction failed"
            );
            anyhow::bail!("Gitea returned {}: {}", status, body);
        }
        Ok(())
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        let url = format!(
            "{}/api/v1/repos/{}/{}/pulls/{}",
            self.base_url, owner, repo, pr_number
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("token {}", self.token))
            .send()
            .await
            .context("Failed to query Gitea pull request")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Gitea pull request query returned {}: {}", status, body);
        }
        #[derive(Deserialize)]
        struct PrResponse {
            head: GiteaPrRef,
            #[serde(default)]
            base: Option<GiteaPrRef>,
            #[serde(default)]
            title: Option<String>,
        }
        #[derive(Deserialize)]
        struct GiteaPrRef {
            sha: String,
            #[serde(rename = "ref")]
            ref_: Option<String>,
            #[serde(default)]
            repo: Option<GiteaPrRepo>,
        }
        #[derive(Deserialize)]
        struct GiteaPrRepo {
            #[serde(default)]
            full_name: Option<String>,
            #[serde(default)]
            clone_url: Option<String>,
        }
        let pr: PrResponse = resp
            .json()
            .await
            .context("Failed to parse Gitea pull request response")?;
        let head_branch = pr.head.ref_.clone().unwrap_or_default();
        let head_full = pr.head.repo.as_ref().and_then(|r| r.full_name.clone());
        let base_full = pr
            .base
            .as_ref()
            .and_then(|b| b.repo.as_ref())
            .and_then(|r| r.full_name.clone());
        let is_fork =
            matches!((head_full.as_deref(), base_full.as_deref()), (Some(h), Some(b)) if h != b);
        let head_clone_url = if is_fork {
            pr.head.repo.as_ref().and_then(|r| r.clone_url.clone())
        } else {
            None
        };
        Ok(Some(PullRequestSnapshot {
            head_sha: pr.head.sha,
            head_branch,
            head_clone_url,
            is_fork,
            title: pr.title,
        }))
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        // The Gitea contents API would stack commits on a never-rebased branch. libgit2 is needing
        // an explicit signature, and the Gradient bot is the fallback without the read:user scope.
        let mut commit = commit.clone();
        if commit.author.is_none() {
            let resolved =
                crate::pr::gitea::authenticated_user(&self.client, &self.base_url, &self.token)
                    .await;
            commit.author = Some(author_or_bot(resolved, &self.base_url));
        }
        crate::git_push::force_push_lock_commit(
            format!("{}/{}/{}.git", self.base_url, owner, repo),
            self.token.clone(),
            String::new(),
            branch.to_owned(),
            base.to_owned(),
            commit,
        )
        .await
    }

    async fn open_or_update_pr(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrRef> {
        crate::pr::gitea::open_or_update_pr(
            &self.client,
            &self.base_url,
            &self.token,
            owner,
            repo,
            head,
            base,
            title,
            body,
        )
        .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        crate::pr::gitea::default_branch(&self.client, &self.base_url, &self.token, owner, repo)
            .await
    }
}

#[derive(Debug)]
pub struct GitlabReporter {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

impl GitlabReporter {
    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self> {
        let raw = base_url.into();
        validate_safe_outbound_url(&raw)
            .map_err(|e| anyhow::anyhow!("Rejected GitLab base_url: {}", e))?;
        Ok(Self {
            base_url: raw.trim_end_matches('/').to_string(),
            token: token.into(),
            client,
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum GitlabState {
    Pending,
    Running,
    Success,
    Failed,
}

impl From<&CiStatus> for GitlabState {
    fn from(s: &CiStatus) -> Self {
        match s {
            CiStatus::Pending | CiStatus::ActionRequired => GitlabState::Pending,
            CiStatus::Running => GitlabState::Running,
            CiStatus::Success => GitlabState::Success,
            CiStatus::Failure => GitlabState::Failed,
            CiStatus::Error => GitlabState::Failed,
        }
    }
}

#[derive(Debug, Serialize)]
struct GitlabStatusPayload<'a> {
    state: GitlabState,
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<&'a str>,
}

fn gitlab_project_id(owner: &str, repo: &str) -> String {
    format!("{}/{}", owner, repo).replace('/', "%2F")
}

fn gitlab_comment_url(base_url: &str, owner: &str, repo: &str, pr_number: u64) -> String {
    let project_id = gitlab_project_id(owner, repo);
    format!(
        "{}/api/v4/projects/{}/merge_requests/{}/notes",
        base_url, project_id, pr_number
    )
}

#[async_trait]
impl CiReporter for GitlabReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let project_id = gitlab_project_id(&report.owner, &report.repo);
        let url = format!(
            "{}/api/v4/projects/{}/statuses/{}",
            self.base_url, project_id, report.sha
        );

        let payload = GitlabStatusPayload {
            state: GitlabState::from(&report.status),
            name: &report.context,
            description: report.description.as_deref(),
            target_url: report.details_url.as_deref(),
        };

        let resp = self
            .client
            .post(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send GitLab status request")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(
                gitlab_url = %url,
                http_status = %status,
                body = %body,
                "GitLab CI status report failed"
            );
            anyhow::bail!("GitLab returned {}: {}", status, body);
        }

        Ok(None)
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        let project_id = gitlab_project_id(owner, repo);
        let encoded_username: String =
            url::form_urlencoded::byte_serialize(username.as_bytes()).collect();
        let url = format!(
            "{}/api/v4/projects/{}/members/all?query={}",
            self.base_url, project_id, encoded_username
        );
        let resp = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
            .context("Failed to query GitLab member access level")?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitLab members query returned {}: {}", status, body);
        }
        #[derive(Deserialize)]
        struct Member {
            username: String,
            access_level: i32,
        }
        let members: Vec<Member> = resp
            .json()
            .await
            .context("Failed to parse GitLab members response")?;
        Ok(members
            .iter()
            .any(|m| m.username.eq_ignore_ascii_case(username) && m.access_level >= 30))
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        let url = gitlab_comment_url(&self.base_url, owner, repo, pr_number);
        let payload = GitHostCommentPayload { body };

        let resp = self
            .client
            .post(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .header("Content-Type", "application/json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send GitLab comment request")?;

        let status = resp.status();
        if !status.is_success() {
            let resp_body = resp.text().await.unwrap_or_default();
            warn!(
                gitlab_url = %url,
                http_status = %status,
                body = %resp_body,
                "GitLab MR comment post failed"
            );
            anyhow::bail!("GitLab returned {}: {}", status, resp_body);
        }
        Ok(())
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        let project_id = gitlab_project_id(&target.owner, &target.repo);
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}/notes/{}/award_emoji",
            self.base_url, project_id, target.pr_number, target.comment_id
        );
        #[derive(Serialize)]
        struct Body<'a> {
            name: &'a str,
        }
        let resp = self
            .client
            .post(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .header("Content-Type", "application/json")
            .json(&Body {
                name: kind.gitlab_name(),
            })
            .send()
            .await
            .context("Failed to send GitLab note award_emoji")?;
        let status = resp.status();
        if status == reqwest::StatusCode::CONFLICT {
            return Ok(());
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(
                gitlab_url = %url,
                http_status = %status,
                body = %body,
                "GitLab MR note reaction failed"
            );
            anyhow::bail!("GitLab returned {}: {}", status, body);
        }
        Ok(())
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        let project_id = gitlab_project_id(owner, repo);
        let url = format!(
            "{}/api/v4/projects/{}/merge_requests/{}",
            self.base_url, project_id, pr_number
        );
        let resp = self
            .client
            .get(&url)
            .header("PRIVATE-TOKEN", &self.token)
            .send()
            .await
            .context("Failed to query GitLab merge request")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitLab MR query returned {}: {}", status, body);
        }
        #[derive(Deserialize)]
        struct MrResponse {
            sha: String,
            source_branch: String,
            #[serde(default)]
            source_project_id: Option<u64>,
            #[serde(default)]
            target_project_id: Option<u64>,
            #[serde(default)]
            title: Option<String>,
        }
        let mr: MrResponse = resp
            .json()
            .await
            .context("Failed to parse GitLab merge request response")?;
        let is_fork = matches!(
            (mr.source_project_id, mr.target_project_id),
            (Some(s), Some(t)) if s != t
        );
        Ok(Some(PullRequestSnapshot {
            head_sha: mr.sha,
            head_branch: mr.source_branch,
            head_clone_url: None,
            is_fork,
            title: mr.title,
        }))
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        let mut commit = commit.clone();
        if commit.author.is_none() {
            let resolved =
                crate::pr::gitlab::authenticated_user(&self.client, &self.base_url, &self.token)
                    .await;
            commit.author = Some(author_or_bot(resolved, &self.base_url));
        }
        crate::git_push::force_push_lock_commit(
            format!("{}/{}/{}.git", self.base_url, owner, repo),
            "oauth2".to_owned(),
            self.token.clone(),
            branch.to_owned(),
            base.to_owned(),
            commit,
        )
        .await
    }

    async fn open_or_update_pr(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrRef> {
        crate::pr::gitlab::open_or_update_pr(
            &self.client,
            &self.base_url,
            &self.token,
            owner,
            repo,
            head,
            base,
            title,
            body,
        )
        .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        crate::pr::gitlab::default_branch(&self.client, &self.base_url, &self.token, owner, repo)
            .await
    }
}

#[derive(Debug)]
pub struct GithubReporter {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

fn github_comment_url(base_url: &str, owner: &str, repo: &str, pr_number: u64) -> String {
    format!(
        "{}/repos/{}/{}/issues/{}/comments",
        base_url, owner, repo, pr_number
    )
}

fn github_reviews_url(base_url: &str, owner: &str, repo: &str, pr_number: u64) -> String {
    format!(
        "{}/repos/{}/{}/pulls/{}/reviews",
        base_url, owner, repo, pr_number
    )
}

#[derive(serde::Serialize)]
struct GithubReviewPayload<'a> {
    event: &'static str,
    body: &'a str,
}

async fn post_github_approval(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    owner: &str,
    repo: &str,
    pr_number: u64,
    body: &str,
) -> Result<()> {
    let url = github_reviews_url(base_url, owner, repo, pr_number);
    let payload = GithubReviewPayload {
        event: "APPROVE",
        body,
    };

    let resp = client
        .post(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .json(&payload)
        .send()
        .await
        .context("Failed to send GitHub PR review request")?;

    let status = resp.status();
    if !status.is_success() {
        let resp_body = resp.text().await.unwrap_or_default();
        warn!(github_url = %url, http_status = %status, body = %resp_body, "GitHub PR approval review failed");
        anyhow::bail!("GitHub returned {}: {}", status, resp_body);
    }

    Ok(())
}

impl GithubReporter {
    const DEFAULT_API_URL: &'static str = "https://api.github.com";

    pub fn new(
        client: reqwest::Client,
        base_url: impl Into<String>,
        token: impl Into<String>,
    ) -> Result<Self> {
        let raw = base_url.into();
        let base_url = if raw.is_empty() {
            Self::DEFAULT_API_URL.to_string()
        } else {
            validate_safe_outbound_url(&raw)
                .map_err(|e| anyhow::anyhow!("Rejected GitHub base_url: {}", e))?;
            raw.trim_end_matches('/').to_string()
        };

        Ok(Self {
            base_url,
            token: token.into(),
            client,
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum GithubState {
    Pending,
    Success,
    Failure,
    Error,
}

impl From<&CiStatus> for GithubState {
    fn from(s: &CiStatus) -> Self {
        match s {
            CiStatus::Pending | CiStatus::Running | CiStatus::ActionRequired => {
                GithubState::Pending
            }
            CiStatus::Success => GithubState::Success,
            CiStatus::Failure => GithubState::Failure,
            CiStatus::Error => GithubState::Error,
        }
    }
}

#[derive(Debug, Serialize)]
struct GithubStatusPayload<'a> {
    state: GithubState,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    context: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<&'a str>,
}

#[async_trait]
impl CiReporter for GithubReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let url = format!(
            "{}/repos/{}/{}/statuses/{}",
            self.base_url, report.owner, report.repo, report.sha
        );

        let payload = GithubStatusPayload {
            state: GithubState::from(&report.status),
            description: report.description.as_deref(),
            context: &report.context,
            target_url: report.details_url.as_deref(),
        };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&payload)
            .send()
            .await
            .context("Failed to send GitHub status request")?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            warn!(
                github_url = %url,
                http_status = %status,
                body = %body,
                "GitHub CI status report failed"
            );
            anyhow::bail!("GitHub returned {}: {}", status, body);
        }

        Ok(None)
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        let url = format!(
            "{}/repos/{}/{}/collaborators/{}/permission",
            self.base_url, owner, repo, username
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("Failed to query GitHub collaborator permission")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub permission query returned {}: {}", status, body);
        }
        let parsed: GithubPermissionResponse = resp
            .json()
            .await
            .context("Failed to parse GitHub permission response")?;
        Ok(matches!(parsed.permission.as_str(), "admin" | "write"))
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        let url = github_comment_url(&self.base_url, owner, repo, pr_number);
        let payload = GitHostCommentPayload { body };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&payload)
            .send()
            .await
            .context("Failed to send GitHub comment request")?;

        let status = resp.status();
        if !status.is_success() {
            let resp_body = resp.text().await.unwrap_or_default();
            warn!(
                github_url = %url,
                http_status = %status,
                body = %resp_body,
                "GitHub PR comment post failed"
            );
            anyhow::bail!("GitHub returned {}: {}", status, resp_body);
        }
        Ok(())
    }

    async fn approve_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        post_github_approval(
            &self.client,
            &self.base_url,
            &self.token,
            owner,
            repo,
            pr_number,
            body,
        )
        .await
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.base_url, owner, repo, pr_number
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("Failed to query GitHub pull request")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub pull request query returned {}: {}", status, body);
        }
        let pr: GithubPrResponse = resp
            .json()
            .await
            .context("Failed to parse GitHub pull request response")?;
        Ok(Some(github_pr_response_to_snapshot(pr)))
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        let url = github_reaction_url(
            &self.base_url,
            &target.owner,
            &target.repo,
            target.comment_id,
        );
        post_github_reaction(&self.client, &url, &self.token, kind).await
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        crate::pr::github::upsert_branch(
            &self.client,
            &self.base_url,
            &self.token,
            owner,
            repo,
            branch,
            base,
            commit,
        )
        .await
    }

    async fn open_or_update_pr(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrRef> {
        crate::pr::github::open_or_update_pr(
            &self.client,
            &self.base_url,
            &self.token,
            owner,
            repo,
            head,
            base,
            title,
            body,
        )
        .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        crate::pr::github::default_branch(&self.client, &self.base_url, &self.token, owner, repo)
            .await
    }
}

fn github_reaction_url(base_url: &str, owner: &str, repo: &str, comment_id: i64) -> String {
    format!(
        "{}/repos/{}/{}/issues/comments/{}/reactions",
        base_url, owner, repo, comment_id
    )
}

async fn post_github_reaction(
    client: &reqwest::Client,
    url: &str,
    token: &str,
    kind: ReactionKind,
) -> Result<()> {
    #[derive(Serialize)]
    struct Body<'a> {
        content: &'a str,
    }
    let resp = client
        .post(url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .json(&Body {
            content: kind.github_content(),
        })
        .send()
        .await
        .context("Failed to send GitHub comment reaction")?;
    let status = resp.status();
    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        warn!(
            github_url = %url,
            http_status = %status,
            body = %body,
            "GitHub comment reaction failed"
        );
        anyhow::bail!("GitHub returned {}: {}", status, body);
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct GithubPermissionResponse {
    permission: String,
}

#[derive(Deserialize)]
struct GithubPrResponse {
    head: GithubPrRef,
    #[serde(default)]
    base: Option<GithubPrRef>,
    #[serde(default)]
    title: Option<String>,
}

#[derive(Deserialize)]
struct GithubPrRef {
    sha: String,
    #[serde(rename = "ref")]
    ref_: String,
    #[serde(default)]
    repo: Option<GithubPrRepo>,
}

#[derive(Deserialize)]
struct GithubPrRepo {
    #[serde(default)]
    full_name: Option<String>,
    #[serde(default)]
    clone_url: Option<String>,
}

fn github_pr_response_to_snapshot(pr: GithubPrResponse) -> PullRequestSnapshot {
    let head_full = pr.head.repo.as_ref().and_then(|r| r.full_name.clone());
    let base_full = pr
        .base
        .as_ref()
        .and_then(|b| b.repo.as_ref())
        .and_then(|r| r.full_name.clone());
    let is_fork =
        matches!((head_full.as_deref(), base_full.as_deref()), (Some(h), Some(b)) if h != b);
    let head_clone_url = if is_fork {
        pr.head.repo.as_ref().and_then(|r| r.clone_url.clone())
    } else {
        None
    };
    PullRequestSnapshot {
        head_sha: pr.head.sha,
        head_branch: pr.head.ref_,
        head_clone_url,
        is_fork,
        title: pr.title,
    }
}

pub struct GithubAppReporter {
    api_base_url: String,
    app_id: u64,
    private_key_pem: String,
    installation_id: i64,
    client: reqwest::Client,
}

impl std::fmt::Debug for GithubAppReporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubAppReporter")
            .field("api_base_url", &self.api_base_url)
            .field("app_id", &self.app_id)
            .field("installation_id", &self.installation_id)
            .finish_non_exhaustive()
    }
}

impl GithubAppReporter {
    const DEFAULT_API_URL: &'static str = "https://api.github.com";

    pub fn new(
        client: reqwest::Client,
        api_base_url: impl Into<String>,
        app_id: u64,
        private_key_pem: impl Into<String>,
        installation_id: i64,
    ) -> Result<Self> {
        let raw = api_base_url.into();
        let api_base_url = if raw.is_empty() {
            Self::DEFAULT_API_URL.to_string()
        } else {
            validate_safe_outbound_url(&raw)
                .map_err(|e| anyhow::anyhow!("Rejected GitHub App api_base_url: {}", e))?;
            raw.trim_end_matches('/').to_string()
        };

        Ok(Self {
            api_base_url,
            app_id,
            private_key_pem: private_key_pem.into(),
            installation_id,
            client,
        })
    }
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum CheckRunStatus {
    Queued,
    InProgress,
    Completed,
}

#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum CheckRunConclusion {
    Success,
    Failure,
    ActionRequired,
}

fn map_ci_status(status: &CiStatus) -> (CheckRunStatus, Option<CheckRunConclusion>) {
    match status {
        CiStatus::Pending => (CheckRunStatus::Queued, None),
        CiStatus::Running => (CheckRunStatus::InProgress, None),
        CiStatus::Success => (CheckRunStatus::Completed, Some(CheckRunConclusion::Success)),
        CiStatus::Failure => (CheckRunStatus::Completed, Some(CheckRunConclusion::Failure)),
        CiStatus::Error | CiStatus::ActionRequired => (
            CheckRunStatus::Completed,
            Some(CheckRunConclusion::ActionRequired),
        ),
    }
}

#[derive(Debug, Serialize)]
struct CheckRunOutput<'a> {
    title: &'a str,
    summary: &'a str,
}

#[derive(Debug, Serialize)]
struct CreateCheckRunPayload<'a> {
    name: &'a str,
    head_sha: &'a str,
    status: CheckRunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    conclusion: Option<CheckRunConclusion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<CheckRunOutput<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actions: Vec<&'a RequestedAction>,
}

#[derive(Debug, Serialize)]
struct UpdateCheckRunPayload<'a> {
    status: CheckRunStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    conclusion: Option<CheckRunConclusion>,
    #[serde(skip_serializing_if = "Option::is_none")]
    details_url: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<CheckRunOutput<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    actions: Vec<&'a RequestedAction>,
}

#[derive(Debug, Deserialize)]
struct CheckRunCreateResponse {
    id: i64,
}

impl GithubAppReporter {
    async fn installation_token(&self) -> Result<String> {
        crate::github_app::get_installation_token(
            &self.client,
            self.app_id,
            &self.private_key_pem,
            self.installation_id,
        )
        .await
        .context("Failed to mint GitHub App installation token")
    }
}

#[async_trait]
impl CiReporter for GithubAppReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let token = crate::github_app::get_installation_token(
            &self.client,
            self.app_id,
            &self.private_key_pem,
            self.installation_id,
        )
        .await
        .context("Failed to mint GitHub App installation token")?;

        let (gh_status, conclusion) = map_ci_status(&report.status);
        let output = report.description.as_deref().map(|s| CheckRunOutput {
            title: report.context.as_str(),
            summary: s,
        });

        if let Some(check_id) = report.existing_check_id {
            let url = format!(
                "{}/repos/{}/{}/check-runs/{}",
                self.api_base_url, report.owner, report.repo, check_id
            );
            let payload = UpdateCheckRunPayload {
                status: gh_status,
                conclusion,
                details_url: report.details_url.as_deref(),
                output,
                actions: report.requested_actions.iter().collect(),
            };
            let resp = self
                .client
                .patch(&url)
                .header("Authorization", format!("Bearer {}", token))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .json(&payload)
                .send()
                .await
                .context("Failed to send GitHub App check-run update")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                warn!(
                    github_url = %url,
                    http_status = %status,
                    body = %body,
                    installation_id = self.installation_id,
                    "GitHub App check-run PATCH failed"
                );
                anyhow::bail!("GitHub App returned {}: {}", status, body);
            }
            Ok(None)
        } else {
            let url = format!(
                "{}/repos/{}/{}/check-runs",
                self.api_base_url, report.owner, report.repo
            );
            let payload = CreateCheckRunPayload {
                name: &report.context,
                head_sha: &report.sha,
                status: gh_status,
                conclusion,
                details_url: report.details_url.as_deref(),
                output,
                actions: report.requested_actions.iter().collect(),
            };
            let resp = self
                .client
                .post(&url)
                .header("Authorization", format!("Bearer {}", token))
                .header("Accept", "application/vnd.github+json")
                .header("X-GitHub-Api-Version", "2022-11-28")
                .json(&payload)
                .send()
                .await
                .context("Failed to send GitHub App check-run create")?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().await.unwrap_or_default();
                warn!(
                    github_url = %url,
                    http_status = %status,
                    body = %body,
                    installation_id = self.installation_id,
                    "GitHub App check-run POST failed"
                );
                anyhow::bail!("GitHub App returned {}: {}", status, body);
            }
            let parsed: CheckRunCreateResponse = resp
                .json()
                .await
                .context("Failed to parse GitHub check-run create response")?;
            Ok(Some(parsed.id))
        }
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        let token = crate::github_app::get_installation_token(
            &self.client,
            self.app_id,
            &self.private_key_pem,
            self.installation_id,
        )
        .await
        .context("Failed to mint GitHub App installation token")?;

        let url = format!(
            "{}/repos/{}/{}/collaborators/{}/permission",
            self.api_base_url, owner, repo, username
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("Failed to query GitHub collaborator permission")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub App permission query returned {}: {}", status, body);
        }
        let parsed: GithubPermissionResponse = resp
            .json()
            .await
            .context("Failed to parse GitHub permission response")?;
        Ok(matches!(parsed.permission.as_str(), "admin" | "write"))
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        let token = crate::github_app::get_installation_token(
            &self.client,
            self.app_id,
            &self.private_key_pem,
            self.installation_id,
        )
        .await
        .context("Failed to mint GitHub App installation token")?;

        let url = github_comment_url(&self.api_base_url, owner, repo, pr_number);
        let payload = GitHostCommentPayload { body };

        let resp = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .json(&payload)
            .send()
            .await
            .context("Failed to send GitHub App comment request")?;

        let status = resp.status();
        if !status.is_success() {
            let resp_body = resp.text().await.unwrap_or_default();
            warn!(
                github_url = %url,
                http_status = %status,
                body = %resp_body,
                installation_id = self.installation_id,
                "GitHub App PR comment post failed"
            );
            anyhow::bail!("GitHub App returned {}: {}", status, resp_body);
        }
        Ok(())
    }

    async fn approve_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        let token = crate::github_app::get_installation_token(
            &self.client,
            self.app_id,
            &self.private_key_pem,
            self.installation_id,
        )
        .await
        .context("Failed to mint GitHub App installation token")?;

        post_github_approval(
            &self.client,
            &self.api_base_url,
            &token,
            owner,
            repo,
            pr_number,
            body,
        )
        .await
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        let token = crate::github_app::get_installation_token(
            &self.client,
            self.app_id,
            &self.private_key_pem,
            self.installation_id,
        )
        .await
        .context("Failed to mint GitHub App installation token")?;

        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            self.api_base_url, owner, repo, pr_number
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", format!("Bearer {}", token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await
            .context("Failed to query GitHub pull request")?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("GitHub App PR query returned {}: {}", status, body);
        }
        let pr: GithubPrResponse = resp
            .json()
            .await
            .context("Failed to parse GitHub pull request response")?;
        Ok(Some(github_pr_response_to_snapshot(pr)))
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        let token = crate::github_app::get_installation_token(
            &self.client,
            self.app_id,
            &self.private_key_pem,
            self.installation_id,
        )
        .await
        .context("Failed to mint GitHub App installation token")?;
        let url = github_reaction_url(
            &self.api_base_url,
            &target.owner,
            &target.repo,
            target.comment_id,
        );
        post_github_reaction(&self.client, &url, &token, kind).await
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        let token = self.installation_token().await?;
        crate::pr::github::upsert_branch(
            &self.client,
            &self.api_base_url,
            &token,
            owner,
            repo,
            branch,
            base,
            commit,
        )
        .await
    }

    async fn open_or_update_pr(
        &self,
        owner: &str,
        repo: &str,
        head: &str,
        base: &str,
        title: &str,
        body: &str,
    ) -> Result<PrRef> {
        let token = self.installation_token().await?;
        crate::pr::github::open_or_update_pr(
            &self.client,
            &self.api_base_url,
            &token,
            owner,
            repo,
            head,
            base,
            title,
            body,
        )
        .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        let token = self.installation_token().await?;
        crate::pr::github::default_branch(&self.client, &self.api_base_url, &token, owner, repo)
            .await
    }
}

pub fn reporter_for_task(
    http: reqwest::Client,
    ci_type: Option<&str>,
    ci_url: Option<&str>,
    ci_token: Option<&str>,
) -> Arc<dyn CiReporter> {
    let Some(token) = ci_token else {
        return Arc::new(NoopCiReporter);
    };

    let git_host = match ci_type.and_then(GitHostType::from_path_segment) {
        Some(git_host) => git_host,
        None => {
            if let Some(unknown) = ci_type {
                warn!(ci_type = %unknown, "Unknown CI reporter type, using noop");
            }
            return Arc::new(NoopCiReporter);
        }
    };

    let registry = GitHostRegistry::with_builtin();
    let Some(provider) = registry.get(git_host) else {
        return Arc::new(NoopCiReporter);
    };

    match provider.build_reporter(http, ci_url, Some(token)) {
        Ok(reporter) => reporter,
        Err(e) => {
            warn!(error = %e, git_host = ?git_host, "Failed to build reporter, falling back to noop");
            Arc::new(NoopCiReporter)
        }
    }
}

pub fn parse_owner_repo(repository_url: &str) -> Option<(String, String)> {
    let url = repository_url
        .strip_prefix("git+")
        .unwrap_or(repository_url);

    let path = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("git://"))
    {
        rest.split_once('/')?.1
    } else {
        let colon_pos = url.find(':')?;
        &url[colon_pos + 1..]
    };

    let path = path.trim_end_matches(".git");
    let mut parts = path.splitn(2, '/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    Some((owner, repo))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_client() -> reqwest::Client {
        gradient_util::http::build_client().expect("build test http client")
    }

    #[test]
    fn author_or_bot_falls_back_to_gradient_bot_on_error() {
        assert_eq!(
            author_or_bot(
                Err(anyhow::anyhow!("403 read:user")),
                "https://codeberg.org"
            ),
            CommitIdent::gradient_bot("https://codeberg.org")
        );
    }

    #[test]
    fn gitlab_project_id_flat_path() {
        assert_eq!(gitlab_project_id("acme", "widgets"), "acme%2Fwidgets");
    }

    #[test]
    fn gitlab_project_id_nested_groups() {
        assert_eq!(gitlab_project_id("group", "sub/repo"), "group%2Fsub%2Frepo");
    }

    #[test]
    fn gitea_reporter_trims_trailing_slash() {
        let r = GiteaReporter::new(test_client(), "https://gitea.example.com/", "tok").unwrap();
        assert_eq!(r.base_url, "https://gitea.example.com");
    }

    #[test]
    fn gitlab_reporter_trims_trailing_slash() {
        let r = GitlabReporter::new(test_client(), "https://gitlab.example.com/", "tok").unwrap();
        assert_eq!(r.base_url, "https://gitlab.example.com");
    }

    #[test]
    fn gitlab_reporter_rejects_aws_metadata_ip() {
        let err = GitlabReporter::new(test_client(), "http://169.254.169.254/", "tok").unwrap_err();
        assert!(format!("{err}").contains("Rejected GitLab base_url"));
    }

    #[test]
    fn gitlab_reporter_rejects_localhost_hostname() {
        assert!(GitlabReporter::new(test_client(), "http://localhost/", "tok").is_err());
    }

    #[test]
    fn gitlab_reporter_rejects_non_http_scheme() {
        assert!(GitlabReporter::new(test_client(), "file:///etc/passwd", "tok").is_err());
    }

    #[test]
    fn github_reporter_empty_url_uses_default() {
        let r = GithubReporter::new(test_client(), "", "tok").unwrap();
        assert_eq!(r.base_url, GithubReporter::DEFAULT_API_URL);
    }

    #[test]
    fn github_reporter_trims_trailing_slash() {
        let r = GithubReporter::new(test_client(), "https://github.example.com/api/v3/", "tok")
            .unwrap();
        assert_eq!(r.base_url, "https://github.example.com/api/v3");
    }

    #[test]
    fn gitea_reporter_rejects_aws_metadata_ip() {
        let err = GiteaReporter::new(test_client(), "http://169.254.169.254/", "tok").unwrap_err();
        assert!(format!("{err}").contains("Rejected Gitea base_url"));
    }

    #[test]
    fn gitea_reporter_rejects_localhost_hostname() {
        assert!(GiteaReporter::new(test_client(), "http://localhost:3000/", "tok").is_err());
    }

    #[test]
    fn gitea_reporter_rejects_loopback_ipv4() {
        assert!(GiteaReporter::new(test_client(), "http://127.0.0.1/", "tok").is_err());
    }

    #[test]
    fn gitea_reporter_rejects_rfc1918() {
        assert!(GiteaReporter::new(test_client(), "http://10.0.0.5/", "tok").is_err());
        assert!(GiteaReporter::new(test_client(), "http://192.168.1.1/", "tok").is_err());
    }

    #[test]
    fn gitea_reporter_rejects_non_http_scheme() {
        assert!(GiteaReporter::new(test_client(), "file:///etc/passwd", "tok").is_err());
        assert!(GiteaReporter::new(test_client(), "ftp://gitea.example.com/", "tok").is_err());
    }

    #[test]
    fn github_reporter_rejects_aws_metadata_ip() {
        let err =
            GithubReporter::new(test_client(), "http://169.254.169.254/api/v3", "tok").unwrap_err();
        assert!(format!("{err}").contains("Rejected GitHub base_url"));
    }

    #[test]
    fn github_reporter_rejects_localhost_hostname() {
        assert!(GithubReporter::new(test_client(), "http://localhost/api/v3", "tok").is_err());
    }

    #[test]
    fn github_reporter_rejects_ipv6_loopback() {
        assert!(GithubReporter::new(test_client(), "http://[::1]/api/v3", "tok").is_err());
    }

    #[test]
    fn github_app_reporter_rejects_aws_metadata_ip() {
        let err = GithubAppReporter::new(test_client(), "http://169.254.169.254/", 1, "pem", 1)
            .unwrap_err();
        assert!(format!("{err}").contains("Rejected GitHub App api_base_url"));
    }

    #[test]
    fn github_app_reporter_empty_url_still_uses_default() {
        let r = GithubAppReporter::new(test_client(), "", 1, "pem", 1).unwrap();
        assert_eq!(r.api_base_url, GithubAppReporter::DEFAULT_API_URL);
    }

    #[test]
    fn reporter_for_task_unsafe_url_falls_back_to_noop() {
        let r = reporter_for_task(
            test_client(),
            Some("gitea"),
            Some("http://169.254.169.254/"),
            Some("tok"),
        );
        assert!(is_noop(&r));
    }

    fn is_noop(r: &Arc<dyn CiReporter>) -> bool {
        format!("{:?}", r).contains("NoopCiReporter")
    }

    #[test]
    fn reporter_for_task_no_token_is_noop() {
        let r = reporter_for_task(test_client(), Some("github"), Some("https://x"), None);
        assert!(is_noop(&r));
    }

    #[test]
    fn reporter_for_task_no_type_is_noop() {
        let r = reporter_for_task(test_client(), None, None, Some("tok"));
        assert!(is_noop(&r));
    }

    #[test]
    fn reporter_for_task_unknown_type_is_noop() {
        let r = reporter_for_task(test_client(), Some("bitbucket"), None, Some("tok"));
        assert!(is_noop(&r));
    }

    #[test]
    fn reporter_for_task_gitea_builds_gitea() {
        let r = reporter_for_task(
            test_client(),
            Some("gitea"),
            Some("https://gitea.example.com"),
            Some("tok"),
        );
        assert!(format!("{:?}", r).contains("GiteaReporter"));
    }

    #[test]
    fn reporter_for_task_gitlab_builds_gitlab() {
        let r = reporter_for_task(
            test_client(),
            Some("gitlab"),
            Some("https://gitlab.example.com"),
            Some("tok"),
        );
        assert!(format!("{:?}", r).contains("GitlabReporter"));
    }

    #[test]
    fn reporter_for_task_github_builds_github() {
        let r = reporter_for_task(test_client(), Some("github"), None, Some("tok"));
        assert!(format!("{:?}", r).contains("GithubReporter"));
    }

    #[test]
    fn parse_owner_repo_https_with_git_suffix() {
        let got = parse_owner_repo("https://github.com/acme/widgets.git");
        assert_eq!(got, Some(("acme".into(), "widgets".into())));
    }

    #[test]
    fn parse_owner_repo_https_without_git_suffix() {
        let got = parse_owner_repo("https://github.com/acme/widgets");
        assert_eq!(got, Some(("acme".into(), "widgets".into())));
    }

    #[test]
    fn parse_owner_repo_http() {
        let got = parse_owner_repo("http://github.com/acme/widgets.git");
        assert_eq!(got, Some(("acme".into(), "widgets".into())));
    }

    #[test]
    fn parse_owner_repo_git_protocol() {
        let got = parse_owner_repo("git://github.com/acme/widgets.git");
        assert_eq!(got, Some(("acme".into(), "widgets".into())));
    }

    #[test]
    fn parse_owner_repo_ssh_scp_style() {
        let got = parse_owner_repo("git@github.com:acme/widgets.git");
        assert_eq!(got, Some(("acme".into(), "widgets".into())));
    }

    #[test]
    fn parse_owner_repo_strips_git_plus_prefix() {
        let got = parse_owner_repo("git+https://github.com/acme/widgets.git");
        assert_eq!(got, Some(("acme".into(), "widgets".into())));
    }

    #[test]
    fn parse_owner_repo_no_path_rejected() {
        assert_eq!(parse_owner_repo("https://github.com"), None);
    }

    #[test]
    fn parse_owner_repo_only_owner_rejected() {
        assert_eq!(parse_owner_repo("https://github.com/acme"), None);
    }

    #[test]
    fn parse_owner_repo_unknown_scheme_rejected() {
        assert_eq!(parse_owner_repo("ftp-no-colon-owner-repo"), None);
    }

    #[test]
    fn parse_owner_repo_ssh_with_subpath() {
        let got = parse_owner_repo("git@gitea.example.com:group/sub/repo.git");
        assert_eq!(got, Some(("group".into(), "sub/repo".into())));
    }

    #[test]
    fn gitlab_comment_url_url_encodes_owner_repo() {
        let url = gitlab_comment_url("https://gitlab.example.com", "group/subgroup", "demo", 7);
        assert_eq!(
            url,
            "https://gitlab.example.com/api/v4/projects/group%2Fsubgroup%2Fdemo/merge_requests/7/notes"
        );
    }

    #[derive(Debug, Default)]
    struct ProbeReporter {
        reported: std::sync::atomic::AtomicBool,
        branch_reads: std::sync::atomic::AtomicUsize,
        unreachable: bool,
    }

    #[async_trait]
    impl CiReporter for ProbeReporter {
        async fn report(&self, _report: &CiReport) -> Result<Option<i64>> {
            self.reported
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(None)
        }

        async fn default_branch(&self, _owner: &str, _repo: &str) -> Result<String> {
            self.branch_reads
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.unreachable {
                anyhow::bail!("Git host unreachable");
            }

            Ok("main".into())
        }
    }

    #[tokio::test]
    async fn verify_reads_repo_without_reporting() {
        use std::sync::atomic::Ordering::SeqCst;

        let ok = ProbeReporter::default();
        ok.verify("acme", "widgets")
            .await
            .expect("verify passes when the repo is reachable");
        assert_eq!(ok.branch_reads.load(SeqCst), 1);
        assert!(
            !ok.reported.load(SeqCst),
            "verify must never post a status to the Git host"
        );

        let down = ProbeReporter {
            unreachable: true,
            ..Default::default()
        };
        let err = down.verify("acme", "widgets").await.unwrap_err();
        assert!(err.to_string().contains("Git host unreachable"));
    }
}
