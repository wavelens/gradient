/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Typed CI reporter configuration for a project.
//!
//! [`ProjectCiConfig`] is constructed from the three raw DB columns
//! (`ci_reporter_type`, `ci_reporter_url`, `ci_reporter_token`) and encodes
//! which combinations are valid at the type level.  Invalid combinations
//! (e.g. Gitea without a URL, any type without a token) map to
//! [`ProjectCiConfig::Disabled`].

use super::reporter::{CiReporter, GiteaReporter, GithubReporter, NoopCiReporter};
use std::sync::Arc;
use tracing::warn;

/// Fully validated CI reporter configuration for a project.
///
/// Constructed via [`ProjectCiConfig::from_db`] from the three raw DB strings.
/// Call [`ProjectCiConfig::into_reporter`] to obtain a usable [`CiReporter`].
#[derive(Debug, Clone)]
pub enum ProjectCiConfig {
    /// No CI reporting — either not configured or the configuration is
    /// incomplete (missing token, unknown type, Gitea without a URL, etc.).
    Disabled,
    /// Report to GitHub (or GitHub Enterprise Server).
    ///
    /// `base_url` is `None` for github.com; set it for GHES
    /// (e.g. `https://github.example.com/api/v3`).
    GitHub {
        base_url: Option<String>,
        token: String,
    },
    /// Report to a self-hosted Gitea instance.
    ///
    /// `base_url` is required — there is no public Gitea SaaS default.
    Gitea { base_url: String, token: String },
}

impl ProjectCiConfig {
    /// Parse raw DB columns into a typed CI configuration.
    ///
    /// `token` must already be decrypted by the caller.  Returns
    /// [`ProjectCiConfig::Disabled`] when:
    /// - `token` is `None`,
    /// - `ci_type` is `None` or unrecognised,
    /// - `ci_type` is `"gitea"` and `ci_url` is `None` or empty.
    pub fn from_db(ci_type: Option<&str>, ci_url: Option<&str>, token: Option<&str>) -> Self {
        let Some(token) = token else {
            return Self::Disabled;
        };
        if token.is_empty() {
            return Self::Disabled;
        }
        let token = token.to_string();

        match ci_type {
            Some("github") => Self::GitHub {
                base_url: ci_url.filter(|u| !u.is_empty()).map(str::to_string),
                token,
            },
            Some("gitea") => {
                let base_url = match ci_url.filter(|u| !u.is_empty()) {
                    Some(u) => u.to_string(),
                    None => {
                        warn!("Gitea CI reporter requires a base_url — disabling CI reporting");
                        return Self::Disabled;
                    }
                };
                Self::Gitea { base_url, token }
            }
            Some(unknown) => {
                warn!(ci_type = %unknown, "Unknown CI reporter type — disabling CI reporting");
                Self::Disabled
            }
            None => Self::Disabled,
        }
    }

    /// Build a concrete [`CiReporter`] from this configuration.
    ///
    /// Returns a [`NoopCiReporter`] for [`ProjectCiConfig::Disabled`] or when
    /// the underlying HTTP client cannot be constructed.
    pub fn into_reporter(self) -> Arc<dyn CiReporter> {
        match self {
            Self::Disabled => Arc::new(NoopCiReporter),
            Self::GitHub { base_url, token } => {
                let url = base_url.unwrap_or_default();
                match GithubReporter::new(url, token) {
                    Ok(r) => Arc::new(r),
                    Err(e) => {
                        warn!(error = %e, "Failed to build GithubReporter, falling back to noop");
                        Arc::new(NoopCiReporter)
                    }
                }
            }
            Self::Gitea { base_url, token } => match GiteaReporter::new(base_url, token) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    warn!(error = %e, "Failed to build GiteaReporter, falling back to noop");
                    Arc::new(NoopCiReporter)
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_no_token() {
        let config = ProjectCiConfig::from_db(Some("github"), Some("https://github.com"), None);
        assert!(matches!(config, ProjectCiConfig::Disabled));
    }

    #[test]
    fn disabled_when_empty_token() {
        let config = ProjectCiConfig::from_db(Some("github"), None, Some(""));
        assert!(matches!(config, ProjectCiConfig::Disabled));
    }

    #[test]
    fn disabled_when_no_type() {
        let config = ProjectCiConfig::from_db(None, None, Some("mytoken"));
        assert!(matches!(config, ProjectCiConfig::Disabled));
    }

    #[test]
    fn disabled_when_unknown_type() {
        let config = ProjectCiConfig::from_db(Some("bitbucket"), None, Some("tok"));
        assert!(matches!(config, ProjectCiConfig::Disabled));
    }

    #[test]
    fn gitea_disabled_without_url() {
        let config = ProjectCiConfig::from_db(Some("gitea"), None, Some("mytoken"));
        assert!(matches!(config, ProjectCiConfig::Disabled));
    }

    #[test]
    fn gitea_disabled_with_empty_url() {
        let config = ProjectCiConfig::from_db(Some("gitea"), Some(""), Some("mytoken"));
        assert!(matches!(config, ProjectCiConfig::Disabled));
    }

    #[test]
    fn gitea_configured_with_url() {
        let config = ProjectCiConfig::from_db(
            Some("gitea"),
            Some("https://gitea.example.com"),
            Some("tok"),
        );
        assert!(matches!(config, ProjectCiConfig::Gitea { .. }));
    }

    #[test]
    fn github_no_url_means_default() {
        let config = ProjectCiConfig::from_db(Some("github"), None, Some("tok"));
        assert!(matches!(
            config,
            ProjectCiConfig::GitHub { base_url: None, .. }
        ));
    }

    #[test]
    fn github_with_enterprise_url() {
        let config = ProjectCiConfig::from_db(
            Some("github"),
            Some("https://github.example.com/api/v3"),
            Some("tok"),
        );
        assert!(matches!(
            config,
            ProjectCiConfig::GitHub {
                base_url: Some(_),
                ..
            }
        ));
    }
}
