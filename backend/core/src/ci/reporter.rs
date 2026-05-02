/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::ci::webhook::validate_webhook_url;
use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use std::time::Duration;
use tracing::warn;

/// Validate a user-supplied base URL for outbound CI API calls.
///
/// Reuses the SSRF guard from the webhook module: rejects non-http(s) schemes
/// and IP literals / hostnames pointing at loopback, link-local (cloud
/// metadata), private, or otherwise-unsafe ranges.
fn validate_safe_outbound_url(url: &str) -> Result<(), String> {
    validate_webhook_url(url).map(|_| ())
}

/// The lifecycle state of a CI check.
///
/// Maps to both the GitHub Checks API (`queued` / `in_progress` / conclusion)
/// and the Gitea Commit Status API (`pending` / `success` / `failure` / `error`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CiStatus {
    /// Work is queued but not yet started.
    Pending,
    /// Work is actively in progress.
    Running,
    /// Completed successfully.
    Success,
    /// Completed with a build/test failure.
    Failure,
    /// Completed with an infrastructure or unexpected error.
    Error,
}

/// All parameters needed to report a CI status to an external provider.
#[derive(Debug, Clone)]
pub struct CiReport {
    /// Repository owner (user or organisation name).
    pub owner: String,
    /// Repository name.
    pub repo: String,
    /// Full 40-character commit SHA.
    pub sha: String,
    /// Stable identifier for this check (e.g. `"gradient/packages.x86_64-linux.hello"`).
    ///
    /// Used as `context` in Gitea and as the check `name` in GitHub.
    pub context: String,
    /// Current lifecycle state of the check.
    pub status: CiStatus,
    /// Short human-readable summary shown inline in the PR/commit view.
    pub description: Option<String>,
    /// URL of the full details page (e.g. the Gradient evaluation page).
    pub details_url: Option<String>,
    /// GitHub `check_run` id when an existing check run should be updated.
    /// Only used by [`GithubAppReporter`]; ignored by all other reporters.
    pub existing_check_id: Option<i64>,
}

/// Abstraction over external CI status providers.
///
/// Implementations report build/evaluation status back to the Git host where
/// the commit lives. Each call may create a new status entry or update an
/// existing one, depending on what the provider supports.
///
/// # Implementors
///
/// - `NoopCiReporter` — silently discards all reports (used when no integration
///   is configured).
/// - `RecordingCiReporter` (test-support) — records every call for assertions.
/// - `GiteaReporter` — Gitea Commit Status API.
/// - `GithubReporter` — GitHub Commit Status API (also works with GitHub Enterprise Server).
#[async_trait]
pub trait CiReporter: Send + Sync + std::fmt::Debug + 'static {
    /// Report or update a CI status for the given commit.
    ///
    /// Returns `Ok(Some(id))` when the call created a new GitHub check run
    /// whose id the caller should persist for future updates. All other
    /// reporters (and PATCHes against an existing check run) return `Ok(None)`.
    async fn report(&self, report: &CiReport) -> Result<Option<i64>>;
}

// ── NoopCiReporter ────────────────────────────────────────────────────────────

/// A no-op `CiReporter` used when no CI integration is configured.
#[derive(Debug)]
pub struct NoopCiReporter;

#[async_trait]
impl CiReporter for NoopCiReporter {
    async fn report(&self, _report: &CiReport) -> Result<Option<i64>> {
        Ok(None)
    }
}

// ── GiteaReporter ─────────────────────────────────────────────────────────────

/// CI reporter that posts commit statuses to a Gitea instance.
///
/// Uses the Gitea Commit Status API:
/// `POST {base_url}/api/v1/repos/{owner}/{repo}/statuses/{sha}`
#[derive(Debug)]
pub struct GiteaReporter {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

impl GiteaReporter {
    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let raw = base_url.into();
        validate_safe_outbound_url(&raw)
            .map_err(|e| anyhow::anyhow!("Rejected Gitea base_url: {}", e))?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("Failed to build HTTP client for GiteaReporter")?;
        Ok(Self {
            base_url: raw.trim_end_matches('/').to_string(),
            token: token.into(),
            client,
        })
    }
}

/// Gitea commit status state strings.
#[derive(Debug, Serialize)]
#[serde(rename_all = "lowercase")]
enum GiteaState {
    Pending,
    Success,
    Error,
    Failure,
    #[allow(dead_code)]
    Warning,
}

impl From<&CiStatus> for GiteaState {
    fn from(s: &CiStatus) -> Self {
        match s {
            CiStatus::Pending | CiStatus::Running => GiteaState::Pending,
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
}

// ── GithubReporter ────────────────────────────────────────────────────────────

/// CI reporter that posts commit statuses to GitHub (or GitHub Enterprise Server).
///
/// Uses the GitHub Commit Status API:
/// `POST {base_url}/repos/{owner}/{repo}/statuses/{sha}`
///
/// Authenticate with a personal access token or a GitHub App installation token
/// that has `repo:status` (or `statuses:write`) permission.
///
/// `base_url` defaults to `https://api.github.com` when empty; override it for
/// GitHub Enterprise Server (e.g. `https://github.example.com/api/v3`).
#[derive(Debug)]
pub struct GithubReporter {
    base_url: String,
    token: String,
    client: reqwest::Client,
}

impl GithubReporter {
    const DEFAULT_API_URL: &'static str = "https://api.github.com";

    pub fn new(base_url: impl Into<String>, token: impl Into<String>) -> Result<Self> {
        let raw = base_url.into();
        let base_url = if raw.is_empty() {
            Self::DEFAULT_API_URL.to_string()
        } else {
            validate_safe_outbound_url(&raw)
                .map_err(|e| anyhow::anyhow!("Rejected GitHub base_url: {}", e))?;
            raw.trim_end_matches('/').to_string()
        };

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("gradient-ci/1.0")
            .build()
            .context("Failed to build HTTP client for GithubReporter")?;

        Ok(Self {
            base_url,
            token: token.into(),
            client,
        })
    }
}

/// GitHub commit status state strings.
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
            CiStatus::Pending | CiStatus::Running => GithubState::Pending,
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
}

// ── GithubAppReporter ────────────────────────────────────────────────────────

/// CI reporter that creates and updates GitHub Check Runs as a GitHub App
/// installation.
///
/// Uses the Check Runs API rather than the Commit Statuses API, so check
/// lifecycle is `queued → in_progress → completed(success|failure|…)`. The
/// caller stores the returned `check_run` id on the row that owns the check
/// (entry_point / evaluation) and passes it back via
/// [`CiReport::existing_check_id`] on the next call so the same check run
/// gets PATCHed in place.
///
/// Mints a fresh installation access token on every report; cheap enough at
/// CI volume to not warrant caching.
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

        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("gradient-ci/1.0")
            .build()
            .context("Failed to build HTTP client for GithubAppReporter")?;

        Ok(Self {
            api_base_url,
            app_id,
            private_key_pem: private_key_pem.into(),
            installation_id,
            client,
        })
    }
}

/// GitHub Check Run `status` field values.
#[derive(Debug, Serialize, Clone, Copy)]
#[serde(rename_all = "snake_case")]
enum CheckRunStatus {
    Queued,
    InProgress,
    Completed,
}

/// GitHub Check Run `conclusion` field values used by Gradient.
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
        CiStatus::Error => (
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
}

#[derive(Debug, Deserialize)]
struct CheckRunCreateResponse {
    id: i64,
}

#[async_trait]
impl CiReporter for GithubAppReporter {
    async fn report(&self, report: &CiReport) -> Result<Option<i64>> {
        let token = crate::ci::github_app::get_installation_token(
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
}

// ── factory ──────────────────────────────────────────────────────────────────

/// Builds a `CiReporter` from a project's CI configuration fields.
///
/// Returns `NoopCiReporter` when CI reporting is not configured or the
/// reporter type is unrecognised.
pub fn reporter_for_project(
    ci_type: Option<&str>,
    ci_url: Option<&str>,
    ci_token: Option<&str>,
) -> Arc<dyn CiReporter> {
    let token = match ci_token {
        Some(t) => t,
        None => return Arc::new(NoopCiReporter),
    };

    let url = ci_url.unwrap_or("");

    match ci_type {
        Some("gitea") => match GiteaReporter::new(url, token) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                warn!(error = %e, "Failed to build GiteaReporter, falling back to noop");
                Arc::new(NoopCiReporter)
            }
        },
        Some("github") => match GithubReporter::new(url, token) {
            Ok(r) => Arc::new(r),
            Err(e) => {
                warn!(error = %e, "Failed to build GithubReporter, falling back to noop");
                Arc::new(NoopCiReporter)
            }
        },
        Some(unknown) => {
            warn!(ci_type = %unknown, "Unknown CI reporter type, using noop");
            Arc::new(NoopCiReporter)
        }
        None => Arc::new(NoopCiReporter),
    }
}

/// Parses `owner` and `repo` out of a repository URL.
///
/// Supports HTTPS (`https://host/owner/repo.git`) and SCP-style SSH
/// (`git@host:owner/repo.git`).  Returns `None` if the URL cannot be parsed.
pub fn parse_owner_repo(repository_url: &str) -> Option<(String, String)> {
    // Normalise: strip git+ prefix added by RepositoryUrl
    let url = repository_url
        .strip_prefix("git+")
        .unwrap_or(repository_url);

    let path = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("git://"))
    {
        // https://host/owner/repo.git → "host/owner/repo.git" → take after first '/'
        rest.split_once('/')?.1
    } else if let Some(colon_pos) = url.find(':') {
        // git@host:owner/repo.git
        &url[colon_pos + 1..]
    } else {
        return None;
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

    // ── State conversions ─────────────────────────────────────────────────────

    #[test]
    fn gitea_state_from_ci_status_all_variants() {
        assert!(matches!(
            GiteaState::from(&CiStatus::Pending),
            GiteaState::Pending
        ));
        assert!(matches!(
            GiteaState::from(&CiStatus::Running),
            GiteaState::Pending
        ));
        assert!(matches!(
            GiteaState::from(&CiStatus::Success),
            GiteaState::Success
        ));
        assert!(matches!(
            GiteaState::from(&CiStatus::Failure),
            GiteaState::Failure
        ));
        assert!(matches!(
            GiteaState::from(&CiStatus::Error),
            GiteaState::Error
        ));
    }

    #[test]
    fn github_state_from_ci_status_all_variants() {
        assert!(matches!(
            GithubState::from(&CiStatus::Pending),
            GithubState::Pending
        ));
        assert!(matches!(
            GithubState::from(&CiStatus::Running),
            GithubState::Pending
        ));
        assert!(matches!(
            GithubState::from(&CiStatus::Success),
            GithubState::Success
        ));
        assert!(matches!(
            GithubState::from(&CiStatus::Failure),
            GithubState::Failure
        ));
        assert!(matches!(
            GithubState::from(&CiStatus::Error),
            GithubState::Error
        ));
    }

    #[test]
    fn gitea_state_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&GiteaState::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&GiteaState::Success).unwrap(),
            "\"success\""
        );
        assert_eq!(
            serde_json::to_string(&GiteaState::Failure).unwrap(),
            "\"failure\""
        );
        assert_eq!(
            serde_json::to_string(&GiteaState::Error).unwrap(),
            "\"error\""
        );
    }

    #[test]
    fn github_state_serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&GithubState::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&GithubState::Success).unwrap(),
            "\"success\""
        );
    }

    // ── Reporter constructors ────────────────────────────────────────────────

    #[test]
    fn gitea_reporter_trims_trailing_slash() {
        let r = GiteaReporter::new("https://gitea.example.com/", "tok").unwrap();
        assert_eq!(r.base_url, "https://gitea.example.com");
    }

    #[test]
    fn gitea_reporter_preserves_no_trailing_slash() {
        let r = GiteaReporter::new("https://gitea.example.com", "tok").unwrap();
        assert_eq!(r.base_url, "https://gitea.example.com");
    }

    #[test]
    fn github_reporter_empty_url_uses_default() {
        let r = GithubReporter::new("", "tok").unwrap();
        assert_eq!(r.base_url, GithubReporter::DEFAULT_API_URL);
    }

    #[test]
    fn github_reporter_custom_url_kept() {
        let r = GithubReporter::new("https://github.example.com/api/v3", "tok").unwrap();
        assert_eq!(r.base_url, "https://github.example.com/api/v3");
    }

    #[test]
    fn github_reporter_trims_trailing_slash() {
        let r = GithubReporter::new("https://github.example.com/api/v3/", "tok").unwrap();
        assert_eq!(r.base_url, "https://github.example.com/api/v3");
    }

    // ── SSRF base_url validation ─────────────────────────────────────────────

    #[test]
    fn gitea_reporter_rejects_aws_metadata_ip() {
        let err = GiteaReporter::new("http://169.254.169.254/", "tok").unwrap_err();
        assert!(format!("{err}").contains("Rejected Gitea base_url"));
    }

    #[test]
    fn gitea_reporter_rejects_localhost_hostname() {
        assert!(GiteaReporter::new("http://localhost:3000/", "tok").is_err());
    }

    #[test]
    fn gitea_reporter_rejects_loopback_ipv4() {
        assert!(GiteaReporter::new("http://127.0.0.1/", "tok").is_err());
    }

    #[test]
    fn gitea_reporter_rejects_rfc1918() {
        assert!(GiteaReporter::new("http://10.0.0.5/", "tok").is_err());
        assert!(GiteaReporter::new("http://192.168.1.1/", "tok").is_err());
    }

    #[test]
    fn gitea_reporter_rejects_non_http_scheme() {
        assert!(GiteaReporter::new("file:///etc/passwd", "tok").is_err());
        assert!(GiteaReporter::new("ftp://gitea.example.com/", "tok").is_err());
    }

    #[test]
    fn github_reporter_rejects_aws_metadata_ip() {
        let err = GithubReporter::new("http://169.254.169.254/api/v3", "tok").unwrap_err();
        assert!(format!("{err}").contains("Rejected GitHub base_url"));
    }

    #[test]
    fn github_reporter_rejects_localhost_hostname() {
        assert!(GithubReporter::new("http://localhost/api/v3", "tok").is_err());
    }

    #[test]
    fn github_reporter_rejects_ipv6_loopback() {
        assert!(GithubReporter::new("http://[::1]/api/v3", "tok").is_err());
    }

    #[test]
    fn github_app_reporter_rejects_aws_metadata_ip() {
        let err = GithubAppReporter::new("http://169.254.169.254/", 1, "pem", 1).unwrap_err();
        assert!(format!("{err}").contains("Rejected GitHub App api_base_url"));
    }

    #[test]
    fn github_app_reporter_empty_url_still_uses_default() {
        let r = GithubAppReporter::new("", 1, "pem", 1).unwrap();
        assert_eq!(r.api_base_url, GithubAppReporter::DEFAULT_API_URL);
    }

    #[test]
    fn reporter_for_project_unsafe_url_falls_back_to_noop() {
        // Bad base_url should not crash callers — the factory logs and returns Noop.
        let r = reporter_for_project(
            Some("gitea"),
            Some("http://169.254.169.254/"),
            Some("tok"),
        );
        assert!(is_noop(&r));
    }

    // ── reporter_for_project factory ─────────────────────────────────────────

    fn is_noop(r: &Arc<dyn CiReporter>) -> bool {
        format!("{:?}", r).contains("NoopCiReporter")
    }

    #[test]
    fn reporter_for_project_no_token_is_noop() {
        let r = reporter_for_project(Some("github"), Some("https://x"), None);
        assert!(is_noop(&r));
    }

    #[test]
    fn reporter_for_project_no_type_is_noop() {
        let r = reporter_for_project(None, None, Some("tok"));
        assert!(is_noop(&r));
    }

    #[test]
    fn reporter_for_project_unknown_type_is_noop() {
        let r = reporter_for_project(Some("bitbucket"), None, Some("tok"));
        assert!(is_noop(&r));
    }

    #[test]
    fn reporter_for_project_gitea_builds_gitea() {
        let r = reporter_for_project(
            Some("gitea"),
            Some("https://gitea.example.com"),
            Some("tok"),
        );
        assert!(format!("{:?}", r).contains("GiteaReporter"));
    }

    #[test]
    fn reporter_for_project_github_builds_github() {
        let r = reporter_for_project(Some("github"), None, Some("tok"));
        assert!(format!("{:?}", r).contains("GithubReporter"));
    }

    // ── parse_owner_repo ─────────────────────────────────────────────────────

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
        // With deeper path, splitn(2) keeps everything after owner/ as the repo name.
        let got = parse_owner_repo("git@gitea.example.com:group/sub/repo.git");
        assert_eq!(got, Some(("group".into(), "sub/repo".into())));
    }
}
