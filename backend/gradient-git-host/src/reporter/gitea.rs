/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{
    CiReport, CiReporter, CiStatus, ContentReactionPayload, GitHostCommentPayload,
    PullRequestSnapshot, ReactionKind, ReactionTarget, author_or_bot, validate_safe_outbound_url,
};
use crate::client::HostClient;
use crate::pr::{BranchCommit, PrRef};
use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug)]
pub struct GiteaReporter {
    client: HostClient,
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
            client: HostClient::gitea(client, raw.trim_end_matches('/'), token),
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

#[derive(Deserialize)]
struct PermissionResponse {
    permission: String,
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

impl From<PrResponse> for PullRequestSnapshot {
    fn from(pr: PrResponse) -> Self {
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
        Self {
            head_sha: pr.head.sha,
            head_branch,
            head_clone_url,
            is_fork,
            title: pr.title,
        }
    }
}

#[async_trait]
impl CiReporter for GiteaReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let payload = GiteaStatusPayload {
            state: GiteaState::from(&report.status),
            description: report.description.as_deref(),
            context: &report.context,
            target_url: report.details_url.as_deref(),
        };
        self.client
            .post(
                &format!(
                    "/repos/{}/{}/statuses/{}",
                    report.owner, report.repo, report.sha
                ),
                &payload,
            )
            .await?;

        Ok(None)
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        let parsed: Option<PermissionResponse> = self
            .client
            .get_json_optional(&format!(
                "/repos/{owner}/{repo}/collaborators/{username}/permission"
            ))
            .await?;

        Ok(parsed.is_some_and(|p| matches!(p.permission.as_str(), "admin" | "owner" | "write")))
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        self.client
            .post(
                &format!("/repos/{owner}/{repo}/issues/{pr_number}/comments"),
                &GitHostCommentPayload { body },
            )
            .await?;

        Ok(())
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        self.client
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

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        let pr: Option<PrResponse> = self
            .client
            .get_json_optional(&format!("/repos/{owner}/{repo}/pulls/{pr_number}"))
            .await?;

        Ok(pr.map(PullRequestSnapshot::from))
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
            let resolved = crate::pr::gitea::authenticated_user(&self.client).await;
            commit.author = Some(author_or_bot(resolved, self.client.base_url()));
        }
        crate::git_push::force_push_lock_commit(
            format!("{}/{}/{}.git", self.client.base_url(), owner, repo),
            self.client.token().to_owned(),
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
        crate::pr::gitea::open_or_update_pr(&self.client, owner, repo, head, base, title, body)
            .await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        crate::pr::gitea::default_branch(&self.client, owner, repo).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::test_client;

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
    fn gitea_reporter_trims_trailing_slash() {
        let r = GiteaReporter::new(test_client(), "https://gitea.example.com/", "tok").unwrap();
        assert_eq!(r.client.base_url(), "https://gitea.example.com");
    }
}
