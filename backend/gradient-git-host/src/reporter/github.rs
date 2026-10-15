/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    CiReport, CiReporter, CiStatus, ContentReactionPayload, CreatedComment, GitHostCommentPayload,
    PullRequestSnapshot, ReactionKind, ReactionTarget, comment_exists, validate_safe_outbound_url,
};
use crate::client::{GITHUB_API_URL, HostClient};
use crate::pr::{BranchCommit, PrRef};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct GithubReporter {
    client: HostClient,
}

impl GithubReporter {
    const DEFAULT_API_URL: &'static str = GITHUB_API_URL;

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
            client: HostClient::github(client, base_url, token),
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

impl From<GithubPrResponse> for PullRequestSnapshot {
    fn from(pr: GithubPrResponse) -> Self {
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
        Self {
            head_sha: pr.head.sha,
            head_branch: pr.head.ref_,
            head_clone_url,
            is_fork,
            title: pr.title,
        }
    }
}

pub(super) async fn post_status(client: &HostClient, report: &CiReport) -> Result<()> {
    let payload = GithubStatusPayload {
        state: GithubState::from(&report.status),
        description: report.description.as_deref(),
        context: &report.context,
        target_url: report.details_url.as_deref(),
    };
    client
        .post(
            &format!(
                "/repos/{}/{}/statuses/{}",
                report.owner, report.repo, report.sha
            ),
            &payload,
        )
        .await?;

    Ok(())
}

pub(super) async fn is_repo_writer(
    client: &HostClient,
    owner: &str,
    repo: &str,
    username: &str,
) -> Result<bool> {
    let parsed: Option<GithubPermissionResponse> = client
        .get_json_optional(&format!(
            "/repos/{owner}/{repo}/collaborators/{username}/permission"
        ))
        .await?;

    Ok(parsed.is_some_and(|p| matches!(p.permission.as_str(), "admin" | "write")))
}

pub(super) async fn post_pr_comment(
    client: &HostClient,
    owner: &str,
    repo: &str,
    pr_number: u64,
    body: &str,
) -> Result<Option<i64>> {
    let created: CreatedComment = client
        .post_json(
            &format!("/repos/{owner}/{repo}/issues/{pr_number}/comments"),
            &GitHostCommentPayload { body },
        )
        .await?;

    Ok(Some(created.id))
}

pub(super) async fn edit_pr_comment(
    client: &HostClient,
    owner: &str,
    repo: &str,
    comment_id: i64,
    body: &str,
) -> Result<bool> {
    comment_exists(
        client
            .patch(
                &format!("/repos/{owner}/{repo}/issues/comments/{comment_id}"),
                &GitHostCommentPayload { body },
            )
            .await,
    )
}

pub(super) async fn get_pull_request(
    client: &HostClient,
    owner: &str,
    repo: &str,
    pr_number: u64,
) -> Result<Option<PullRequestSnapshot>> {
    let pr: Option<GithubPrResponse> = client
        .get_json_optional(&format!("/repos/{owner}/{repo}/pulls/{pr_number}"))
        .await?;

    Ok(pr.map(PullRequestSnapshot::from))
}

pub(super) async fn add_reaction(
    client: &HostClient,
    target: &ReactionTarget,
    kind: ReactionKind,
) -> Result<()> {
    client
        .post(
            &format!(
                "/repos/{}/{}/issues/comments/{}/reactions",
                target.owner, target.repo, target.comment_id
            ),
            &ContentReactionPayload {
                content: kind.github_content(),
            },
        )
        .await?;

    Ok(())
}

#[async_trait]
impl CiReporter for GithubReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        post_status(&self.client, report).await?;

        Ok(None)
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        is_repo_writer(&self.client, owner, repo, username).await
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<Option<i64>> {
        post_pr_comment(&self.client, owner, repo, pr_number, body).await
    }

    async fn edit_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        _pr_number: u64,
        comment_id: i64,
        body: &str,
    ) -> Result<bool> {
        edit_pr_comment(&self.client, owner, repo, comment_id, body).await
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        get_pull_request(&self.client, owner, repo, pr_number).await
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        add_reaction(&self.client, target, kind).await
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        crate::pr::github::upsert_branch(&self.client, owner, repo, branch, base, commit).await
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
        crate::pr::github::open_or_update_pr(&self.client, owner, repo, head, base, title, body)
            .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        crate::pr::github::default_branch(&self.client, owner, repo).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::test_client;

    #[test]
    fn github_reporter_empty_url_uses_default() {
        let r = GithubReporter::new(test_client(), "", "tok").unwrap();
        assert_eq!(r.client.base_url(), GithubReporter::DEFAULT_API_URL);
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
    fn github_reporter_trims_trailing_slash() {
        let r = GithubReporter::new(test_client(), "https://github.example.com/api/v3/", "tok")
            .unwrap();
        assert_eq!(r.client.base_url(), "https://github.example.com/api/v3");
    }
}
