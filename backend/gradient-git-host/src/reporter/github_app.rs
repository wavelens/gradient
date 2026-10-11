/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::github;
use super::{
    CiReport, CiReporter, CiStatus, PullRequestSnapshot, ReactionKind, ReactionTarget,
    RequestedAction, validate_safe_outbound_url,
};
use crate::client::{GITHUB_API_URL, HostClient};
use crate::pr::{BranchCommit, PrRef};
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

pub struct GithubAppReporter {
    api_base_url: String,
    app_id: u64,
    private_key_pem: String,
    installation_id: i64,
    http: reqwest::Client,
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
    const DEFAULT_API_URL: &'static str = GITHUB_API_URL;

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
            http: client,
        })
    }

    async fn installation_client(&self) -> Result<HostClient> {
        let token = crate::github_app::get_installation_token(
            &self.http,
            self.app_id,
            &self.private_key_pem,
            self.installation_id,
        )
        .await
        .context("mint GitHub App installation token")?;

        Ok(HostClient::github(
            self.http.clone(),
            self.api_base_url.as_str(),
            token,
        ))
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

#[async_trait]
impl CiReporter for GithubAppReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let client = self.installation_client().await?;

        let (gh_status, conclusion) = map_ci_status(&report.status);
        let output = report.description.as_deref().map(|s| CheckRunOutput {
            title: report.context.as_str(),
            summary: s,
        });

        if let Some(check_id) = report.existing_check_id {
            let payload = UpdateCheckRunPayload {
                status: gh_status,
                conclusion,
                details_url: report.details_url.as_deref(),
                output,
                actions: report.requested_actions.iter().collect(),
            };
            client
                .patch(
                    &format!(
                        "/repos/{}/{}/check-runs/{}",
                        report.owner, report.repo, check_id
                    ),
                    &payload,
                )
                .await?;

            Ok(None)
        } else {
            let payload = CreateCheckRunPayload {
                name: &report.context,
                head_sha: &report.sha,
                status: gh_status,
                conclusion,
                details_url: report.details_url.as_deref(),
                output,
                actions: report.requested_actions.iter().collect(),
            };
            let created: CheckRunCreateResponse = client
                .post_json(
                    &format!("/repos/{}/{}/check-runs", report.owner, report.repo),
                    &payload,
                )
                .await?;

            Ok(Some(created.id))
        }
    }

    async fn is_repo_writer(&self, owner: &str, repo: &str, username: &str) -> Result<bool> {
        let client = self.installation_client().await?;
        github::is_repo_writer(&client, owner, repo, username).await
    }

    async fn post_pr_comment(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
        body: &str,
    ) -> Result<()> {
        let client = self.installation_client().await?;
        github::post_pr_comment(&client, owner, repo, pr_number, body).await
    }

    async fn get_pull_request(
        &self,
        owner: &str,
        repo: &str,
        pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        let client = self.installation_client().await?;
        github::get_pull_request(&client, owner, repo, pr_number).await
    }

    async fn add_reaction(&self, target: &ReactionTarget, kind: ReactionKind) -> Result<()> {
        let client = self.installation_client().await?;
        github::add_reaction(&client, target, kind).await
    }

    async fn upsert_branch(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        base: &str,
        commit: &BranchCommit,
    ) -> Result<String> {
        let client = self.installation_client().await?;
        crate::pr::github::upsert_branch(&client, owner, repo, branch, base, commit).await
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
        let client = self.installation_client().await?;
        crate::pr::github::open_or_update_pr(&client, owner, repo, head, base, title, body).await
    }

    async fn default_branch(&self, owner: &str, repo: &str) -> Result<String> {
        let client = self.installation_client().await?;
        crate::pr::github::default_branch(&client, owner, repo).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reporter::test_client;

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
}
