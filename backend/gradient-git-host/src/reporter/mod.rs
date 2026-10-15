/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::pr::{BranchCommit, CommitIdent, PrRef};
use crate::registry::GitHostRegistry;
use anyhow::Result;
use async_trait::async_trait;
use gradient_types::GitHostType;
use gradient_util::http_validation::{WebhookUrlError, validate_webhook_url};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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
        warn!(error = %e, "resolving Git host commit author; using Gradient bot default");
        CommitIdent::gradient_bot(base_url)
    })
}

fn comment_exists(edited: Result<(), crate::client::HostError>) -> Result<bool> {
    match edited {
        Err(e) if e.status() == Some(reqwest::StatusCode::NOT_FOUND) => Ok(false),
        other => other.map(|()| true).map_err(Into::into),
    }
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
        .or_else(|| url.strip_prefix("ssh://"))
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
fn test_client() -> reqwest::Client {
    gradient_util::http::build_client().expect("build test http client")
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

    fn is_noop(r: &Arc<dyn CiReporter>) -> bool {
        format!("{:?}", r).contains("NoopCiReporter")
    }

    #[test]
    fn reporter_for_task_picks_by_type_and_falls_back_to_noop() {
        assert!(is_noop(&reporter_for_task(
            test_client(),
            Some("gitea"),
            Some("http://169.254.169.254/"),
            Some("tok")
        )));
        assert!(is_noop(&reporter_for_task(
            test_client(),
            Some("github"),
            Some("https://x"),
            None
        )));
        assert!(is_noop(&reporter_for_task(
            test_client(),
            None,
            None,
            Some("tok")
        )));
        assert!(is_noop(&reporter_for_task(
            test_client(),
            Some("bitbucket"),
            None,
            Some("tok")
        )));

        let cases = [
            ("gitea", Some("https://gitea.example.com"), "GiteaReporter"),
            (
                "gitlab",
                Some("https://gitlab.example.com"),
                "GitlabReporter",
            ),
            ("github", None, "GithubReporter"),
        ];
        for (kind, url, marker) in cases {
            let r = reporter_for_task(test_client(), Some(kind), url, Some("tok"));
            assert!(format!("{:?}", r).contains(marker), "{kind}");
        }
    }

    #[test]
    fn parse_owner_repo_reads_the_owner_and_repo_from_each_transport() {
        for url in [
            "https://github.com/acme/widgets.git",
            "https://github.com/acme/widgets",
            "http://github.com/acme/widgets.git",
            "git://github.com/acme/widgets.git",
            "git@github.com:acme/widgets.git",
            "git+https://github.com/acme/widgets.git",
            "git+ssh://git@github.com/acme/widgets.git",
        ] {
            assert_eq!(
                parse_owner_repo(url),
                Some(("acme".into(), "widgets".into())),
                "{url}"
            );
        }
        assert_eq!(
            parse_owner_repo("ssh://forgejo@gitea.home.lan:2222/internal/infra-home"),
            Some(("internal".into(), "infra-home".into()))
        );
        assert_eq!(
            parse_owner_repo("git@gitea.example.com:group/sub/repo.git"),
            Some(("group".into(), "sub/repo".into()))
        );
    }

    #[test]
    fn parse_owner_repo_rejects_urls_without_a_repo_path() {
        for url in [
            "https://github.com",
            "https://github.com/acme",
            "ftp-no-colon-owner-repo",
        ] {
            assert_eq!(parse_owner_repo(url), None, "{url}");
        }
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
