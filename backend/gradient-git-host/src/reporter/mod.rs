/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::pr::{BranchCommit, CommitIdent, PrRef};
use anyhow::Result;
use async_trait::async_trait;
pub use gradient_types::parse_owner_repo;
use gradient_util::http::HttpClientForUserSuppliedUrls;
use gradient_util::http_base_url::ValidatedHttpBaseUrl;
use gradient_util::http_validation::parse_and_check_allowed_outbound_url;
use serde::{Deserialize, Serialize};
use tracing::warn;

mod gitea;
mod github;
mod github_app;
mod gitlab;

pub use gitea::GiteaReporter;
pub use github::GithubReporter;
pub use github_app::GithubAppReporter;
pub use gitlab::GitlabReporter;

fn author_or_bot(resolved: Result<CommitIdent>, base_url: &str) -> CommitIdent {
    resolved.unwrap_or_else(|e| {
        warn!(
            error = format!("{e:#}"),
            "resolving Git host commit author; using Gradient bot default"
        );
        CommitIdent::gradient_bot(base_url)
    })
}

fn comment_exists(edited: Result<(), crate::client::HostError>) -> Result<bool> {
    match edited {
        Err(e) if e.status() == Some(reqwest::StatusCode::NOT_FOUND) => Ok(false),
        other => other.map(|()| true).map_err(Into::into),
    }
}

fn parse_user_git_host_base_url_and_check_allowlist(
    http: &HttpClientForUserSuppliedUrls,
    raw: &str,
) -> Result<ValidatedHttpBaseUrl> {
    let base_url: ValidatedHttpBaseUrl = raw.parse()?;
    parse_and_check_allowed_outbound_url(base_url.as_str(), http.allowlist())?;
    Ok(base_url)
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
    pub head_clone_url: Option<gradient_types::RepositoryUrl>,
    pub is_fork: Option<bool>,
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
    ) -> Result<Option<i64>> {
        Ok(None)
    }

    async fn edit_pr_comment(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _comment_id: i64,
        _body: &str,
    ) -> Result<bool> {
        Ok(false)
    }

    async fn get_pull_request(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
    ) -> Result<Option<PullRequestSnapshot>> {
        Ok(None)
    }

    /// Return the commit of the newest deciding review (APPROVED or REQUEST_CHANGES) for this
    /// user. If none, nothing is approved. Match reviews by stable user ID. The review must still
    /// carry the login of the webhook sender. No approval for a login that a renamed reviewer gave
    /// up and another user took.
    async fn get_newest_undismissed_approval_review_commit_id_for_user(
        &self,
        _owner: &str,
        _repo: &str,
        _pr_number: u64,
        _user_id: i64,
        _webhook_sender_login: &str,
    ) -> Result<Option<Vec<u8>>> {
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

#[derive(Debug, Serialize)]
struct GitHostCommentPayload<'a> {
    body: &'a str,
}

#[derive(Debug, Deserialize)]
struct CreatedComment {
    id: i64,
}

#[derive(Debug, Serialize)]
struct ContentReactionPayload<'a> {
    content: &'a str,
}

#[cfg(test)]
fn test_client() -> HttpClientForUserSuppliedUrls {
    HttpClientForUserSuppliedUrls::new(Default::default()).expect("build test http client")
}

#[cfg(test)]
mod tests {
    use super::*;

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
