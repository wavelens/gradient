/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Resolve named integrations for organizations and projects.
//!
//! The `integration` table stores per-org named records of forge integrations.
//! Each project can reference a single inbound and a single outbound
//! integration via the `project_integration` link table.

use super::reporter::{CiReporter, GiteaReporter, GithubAppReporter, NoopCiReporter};
use super::webhook::decrypt_webhook_secret;
use crate::types::*;
use sea_orm::EntityTrait;
use std::fs;
use std::sync::Arc;
use tracing::warn;
use uuid::Uuid;

/// Numeric encoding of `integration.kind`.
#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationKind {
    Inbound = 0,
    Outbound = 1,
}

impl IntegrationKind {
    pub fn as_i16(self) -> i16 {
        self as i16
    }
}

/// Numeric encoding of `integration.forge_type`.
#[repr(i16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ForgeType {
    Gitea = 0,
    Forgejo = 1,
    GitLab = 2,
    GitHub = 3,
}

impl ForgeType {
    pub fn as_i16(self) -> i16 {
        self as i16
    }

    pub fn from_i16(v: i16) -> Option<Self> {
        match v {
            0 => Some(Self::Gitea),
            1 => Some(Self::Forgejo),
            2 => Some(Self::GitLab),
            3 => Some(Self::GitHub),
            _ => None,
        }
    }

    pub fn from_path_segment(s: &str) -> Option<Self> {
        match s {
            "gitea" => Some(Self::Gitea),
            "forgejo" => Some(Self::Forgejo),
            "gitlab" => Some(Self::GitLab),
            "github" => Some(Self::GitHub),
            _ => None,
        }
    }
}

/// Build a CI reporter for a project's configured **outbound** integration.
///
/// Returns [`NoopCiReporter`] when:
/// - the project has no `project_integration` row,
/// - the row has no `outbound_integration`,
/// - the integration is unreachable or its token cannot be decrypted,
/// - the forge type does not support outbound reporting yet (GitLab, GitHub App).
pub async fn resolve_outbound_reporter_for_project(
    state: &Arc<ServerState>,
    project_id: Uuid,
) -> Arc<dyn CiReporter> {
    use sea_orm::ColumnTrait;
    use sea_orm::QueryFilter;

    let link = match EProjectIntegration::find_by_id(project_id)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(l)) => Some(l),
        Ok(None) => None,
        Err(e) => {
            warn!(error = %e, %project_id, "DB error looking up project_integration");
            None
        }
    };

    // GitHub App auto-detect: when the org has a stored
    // `github_installation_id`, fall back to the server-wide App credentials
    // regardless of whether `project_integration.outbound_integration` is
    // populated. The presence of an installation id is the org's opt-in.
    let outbound_id = link.as_ref().and_then(|l| l.outbound_integration);
    if outbound_id.is_none() {
        if let Some(reporter) = build_github_app_reporter_for_project(state, project_id).await {
            return reporter;
        }
        return Arc::new(NoopCiReporter);
    }
    let outbound_id = outbound_id.unwrap();

    let integration = match EIntegration::find_by_id(outbound_id)
        .filter(CIntegration::Kind.eq(IntegrationKind::Outbound.as_i16()))
        .one(&state.worker_db)
        .await
    {
        Ok(Some(i)) => i,
        Ok(None) => return Arc::new(NoopCiReporter),
        Err(e) => {
            warn!(error = %e, %outbound_id, "DB error looking up outbound integration");
            return Arc::new(NoopCiReporter);
        }
    };

    let forge = match ForgeType::from_i16(integration.forge_type) {
        Some(f) => f,
        None => return Arc::new(NoopCiReporter),
    };

    // Decrypt access token if present.
    let token = match integration.access_token.as_deref() {
        Some(enc) => match decrypt_webhook_secret(&state.config.secrets.crypt_secret_file, enc) {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(error = %e, integration_id = %integration.id, "Failed to decrypt integration access token");
                None
            }
        },
        None => None,
    };

    match forge {
        ForgeType::Gitea | ForgeType::Forgejo => {
            let Some(base_url) = integration
                .endpoint_url
                .as_deref()
                .filter(|s| !s.is_empty())
            else {
                warn!(integration_id = %integration.id, "Gitea/Forgejo outbound integration missing endpoint_url");
                return Arc::new(NoopCiReporter);
            };
            let Some(token) = token else {
                return Arc::new(NoopCiReporter);
            };
            match GiteaReporter::new(base_url.to_string(), token.expose().to_string()) {
                Ok(r) => Arc::new(r),
                Err(e) => {
                    warn!(error = %e, "Failed to build GiteaReporter");
                    Arc::new(NoopCiReporter)
                }
            }
        }
        ForgeType::GitHub => {
            // Per-integration `forge_type=github` rows are legacy/no-op:
            // outbound GitHub credentials always come from the server-wide
            // GitHub App via `build_github_app_reporter_for_project`, which
            // we already attempted above when no UUID was set. Falling
            // through to it here covers the "operator created a github
            // outbound row before the UI removed the option" case.
            build_github_app_reporter_for_project(state, project_id)
                .await
                .unwrap_or_else(|| Arc::new(NoopCiReporter))
        }
        ForgeType::GitLab => {
            // TODO: implement GitLabReporter.
            Arc::new(NoopCiReporter)
        }
    }
}

/// Builds a [`GithubAppReporter`] for a project when:
///   1. the server has the GitHub App fully configured,
///   2. the project's organization has a stored `github_installation_id`.
///
/// Returns `None` when any precondition isn't satisfied so the caller can
/// fall back to a noop or a different reporter.
///
/// The project's repo URL is **not** consulted: a single GitHub App
/// installation is scoped to one GitHub org/account, and we trust the
/// `github_installation_id` stored on the Gradient org rather than re-deriving
/// from URLs (which would require parsing Enterprise hosts, etc.).
async fn build_github_app_reporter_for_project(
    state: &Arc<ServerState>,
    project_id: Uuid,
) -> Option<Arc<dyn CiReporter>> {
    let github_app = state.config.github_app.clone()?;

    let project = EProject::find_by_id(project_id)
        .one(&state.worker_db)
        .await
        .ok()
        .flatten()?;

    if !is_github_repository_url(&project.repository) {
        return None;
    }

    let org = EOrganization::find_by_id(project.organization)
        .one(&state.worker_db)
        .await
        .ok()
        .flatten()?;

    let installation_id = org.github_installation_id?;

    let pem = match fs::read_to_string(&github_app.private_key_file) {
        Ok(s) => s,
        Err(e) => {
            warn!(
                error = %e,
                path = %github_app.private_key_file,
                "Failed to read GitHub App private key for outbound CI reporting"
            );
            return None;
        }
    };

    // GitHub Enterprise support deferred — no production user yet. When
    // adding it, derive `api_base_url` from the installation account host or
    // from a server-config field instead of hardcoding the empty default.
    match GithubAppReporter::new("", github_app.app_id, pem, installation_id) {
        Ok(r) => Some(Arc::new(r)),
        Err(e) => {
            warn!(error = %e, "Failed to build GithubAppReporter");
            None
        }
    }
}

/// Returns `true` when `url` points at github.com.
///
/// Accepts the URL shapes that `parse_owner_repo` does (HTTPS, HTTP, git://,
/// SCP-style SSH, and an optional `git+` prefix) plus `ssh://` URLs. The host
/// match is exact: `github.com` and `*.github.com` only.
fn is_github_repository_url(url: &str) -> bool {
    let url = url.strip_prefix("git+").unwrap_or(url);

    let host_and_rest = if let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .or_else(|| url.strip_prefix("git://"))
        .or_else(|| url.strip_prefix("ssh://"))
    {
        rest
    } else if let Some(at_pos) = url.find('@')
        && url[at_pos + 1..].contains(':')
    {
        &url[at_pos + 1..]
    } else {
        return false;
    };

    let host = host_and_rest
        .split(|c| c == '/' || c == ':')
        .next()
        .unwrap_or("");
    let host = host.rsplit('@').next().unwrap_or(host);

    host.eq_ignore_ascii_case("github.com") || host.to_ascii_lowercase().ends_with(".github.com")
}

#[cfg(test)]
mod tests {
    use super::is_github_repository_url;

    #[test]
    fn github_https_is_github() {
        assert!(is_github_repository_url("https://github.com/acme/widgets.git"));
        assert!(is_github_repository_url("https://github.com/acme/widgets"));
    }

    #[test]
    fn github_ssh_scp_is_github() {
        assert!(is_github_repository_url("git@github.com:acme/widgets.git"));
    }

    #[test]
    fn github_git_plus_https_is_github() {
        assert!(is_github_repository_url(
            "git+https://github.com/acme/widgets.git"
        ));
    }

    #[test]
    fn github_case_insensitive() {
        assert!(is_github_repository_url("https://GitHub.com/acme/widgets"));
    }

    #[test]
    fn gitea_https_is_not_github() {
        assert!(!is_github_repository_url(
            "https://git.wavelens.io/Wavelens/nix-dotfiles.git"
        ));
    }

    #[test]
    fn gitea_ssh_scp_is_not_github() {
        assert!(!is_github_repository_url(
            "gitea@git.wavelens.io:Wavelens/nix-dotfiles.git"
        ));
    }

    #[test]
    fn gitea_ssh_url_is_not_github() {
        // Reproduces the bug: ssh://gitea@git.wavelens.io:12/... was being
        // routed through the GitHub App reporter.
        assert!(!is_github_repository_url(
            "ssh://gitea@git.wavelens.io:12/Wavelens/nix-dotfiles"
        ));
    }

    #[test]
    fn lookalike_host_is_not_github() {
        assert!(!is_github_repository_url("https://github.com.evil/x/y"));
        assert!(!is_github_repository_url("https://notgithub.com/x/y"));
    }

    #[test]
    fn github_subdomain_is_github() {
        assert!(is_github_repository_url("https://api.github.com/x/y"));
    }
}
