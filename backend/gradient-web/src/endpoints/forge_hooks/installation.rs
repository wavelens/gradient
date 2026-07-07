/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! GitHub App installation binding and repository-to-integration resolution.

use super::payloads::GitHubInstallationPayload;
use gradient_ci::parse_owner_repo;
use gradient_core::ServerState;
use gradient_types::*;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::sync::Arc;
use tracing::{debug, info, warn};

pub(super) async fn handle_github_installation(state: &Arc<ServerState>, body: &[u8]) {
    let payload: GitHubInstallationPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse GitHub installation payload");
            return;
        }
    };

    if payload.action == "deleted" {
        clear_installation_id(state, payload.installation.id).await;
        return;
    }

    store_installation_id(state, &payload).await;
}

async fn clear_installation_id(state: &Arc<ServerState>, installation_id: i64) {
    use gradient_entity::github_installation::{Column as Col, Entity as E};
    if let Err(e) = E::delete_many()
        .filter(Col::InstallationId.eq(installation_id))
        .exec(&state.web_db)
        .await
    {
        warn!(error = %e, installation_id, "Failed to delete github_installation rows");
    }
}

async fn store_installation_id(state: &Arc<ServerState>, payload: &GitHubInstallationPayload) {
    use std::collections::HashSet;

    let github_login = payload.installation.account.login.as_str();
    let installation_id = payload.installation.id;
    let installed = payload.installed_full_names();

    if installed.is_empty() {
        debug!(
            installation_id,
            github_login, "GitHub App install carried no repository list; nothing to bind"
        );
        return;
    }

    // Bind to every org owning a project whose parsed `owner/repo` matches an
    // installed repo, so flake shorthand and every clone-URL form match alike.
    let owner_org_ids: HashSet<OrganizationId> = EProject::find()
        .filter(CProject::Repository.contains("github"))
        .all(&state.web_db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|p| github_full_name(&p.repository).is_some_and(|n| installed.contains(&n)))
        .map(|p| p.organization)
        .collect();

    if owner_org_ids.is_empty() {
        let sender_login = payload
            .sender
            .as_ref()
            .map(|s| s.login.as_str())
            .unwrap_or("unknown");
        warn!(
            github_login,
            sender = %sender_login,
            installation_id,
            "GitHub App installed but no Gradient project tracks an installed repository"
        );

        return;
    }

    let orgs = EOrganization::find()
        .filter(COrganization::Id.is_in(owner_org_ids))
        .all(&state.web_db)
        .await
        .unwrap_or_default();

    for org in orgs {
        let org_id = org.id;
        let creator = org.created_by;
        let inst = match gradient_ci::upsert_github_installation(
            &state.web_db,
            org_id,
            installation_id,
            Some(github_login),
            creator,
        )
        .await
        {
            Ok(id) => id,
            Err(e) => {
                warn!(error = %e, installation_id, %org_id, "Failed to upsert github_installation");
                continue;
            }
        };

        info!(installation_id, %org_id, github_login = %github_login, "GitHub App installed on organization");
        let name = gradient_ci::github_integration_name(Some(github_login), installation_id);
        if let Err(e) = gradient_ci::ensure_github_app_integrations(
            &state.web_db,
            org_id,
            inst,
            &name,
            "GitHub",
            creator,
        )
        .await
        {
            warn!(error = %e, %org_id, "Failed to materialise GitHub App integration rows");
        }
    }
}

/// Lowercased `owner/repo` for a github.com repository, or `None` for a
/// non-github URL. Recognizes https, SCP-style SSH, and the flake shorthand.
fn github_full_name(repo_url: &str) -> Option<String> {
    let lower = repo_url.to_ascii_lowercase();
    let is_github = lower.contains("github.com")
        || lower.starts_with("github:")
        || lower.starts_with("git+github:");
    if !is_github {
        return None;
    }

    parse_owner_repo(repo_url).map(|(owner, repo)| format!("{owner}/{repo}").to_ascii_lowercase())
}

/// Canonical form for matching `project.repository` against forge-reported URLs:
/// strips `.git`/trailing slash and rewrites `git@host:owner/repo` SSH to https.
pub(super) fn normalize_repo_url(url: &str) -> String {
    let s = url.trim().trim_end_matches('/');
    let s = s.strip_suffix(".git").unwrap_or(s);
    if let Some(rest) = s.strip_prefix("git@")
        && let Some((host, path)) = rest.split_once(':')
    {
        return format!("https://{}/{}", host, path);
    }
    s.to_string()
}

fn repo_identity(url: &str) -> Option<String> {
    parse_owner_repo(url).map(|(owner, repo)| format!("{owner}/{repo}").to_ascii_lowercase())
}

/// Whether a webhook event from `event_repo_urls` targets a project tracking
/// `project_repository`. An org-wide inbound integration (a GitHub App spans the
/// whole org) would otherwise fan out to sibling projects. Empty urls match all.
pub(super) fn event_repo_matches_project(
    event_repo_urls: &[String],
    project_repository: &str,
) -> bool {
    let mut keys = event_repo_urls
        .iter()
        .filter_map(|u| repo_identity(u))
        .peekable();
    if keys.peek().is_none() {
        return true;
    }

    match repo_identity(project_repository) {
        Some(target) => keys.any(|k| k == target),
        None => false,
    }
}

/// Resolve a GitHub App webhook to the inbound GitHub integrations whose org
/// owns a project matching one of `repository_urls`. A single installation can
/// serve multiple orgs, so the repo-URL gate selects only the matching ones.
pub(super) async fn resolve_github_app_targets(
    state: &Arc<ServerState>,
    installation_id: i64,
    repository_urls: &[String],
    client_ip: std::net::IpAddr,
) -> Vec<IntegrationId> {
    use crate::ip_allowlist::is_allowed as ip_allowed;
    use gradient_ci::IntegrationKind;
    use std::collections::HashSet;

    use gradient_entity::github_installation::{Column as GiCol, Entity as EGi};

    let installs = EGi::find()
        .filter(GiCol::InstallationId.eq(installation_id))
        .all(&state.web_db)
        .await
        .unwrap_or_default();

    if installs.is_empty() {
        return Vec::new();
    }

    let webhook_urls: HashSet<String> = repository_urls
        .iter()
        .map(|u| normalize_repo_url(u))
        .collect();

    let mut integrations = Vec::new();
    for inst in installs {
        let org_id = inst.organization;
        let projects = match EProject::find()
            .filter(CProject::Organization.eq(org_id))
            .all(&state.web_db)
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                warn!(error = %e, %org_id, "resolve_github_app_targets: project lookup failed");
                continue;
            }
        };
        let has_match = projects
            .iter()
            .any(|p| webhook_urls.contains(&normalize_repo_url(&p.repository)));
        if !has_match {
            continue;
        }
        let integration = EIntegration::find()
            .filter(CIntegration::Organization.eq(org_id))
            .filter(CIntegration::Kind.eq(i16::from(IntegrationKind::Inbound)))
            .filter(CIntegration::ForgeType.eq(i16::from(gradient_types::ForgeType::GitHub)))
            .filter(CIntegration::GithubInstallation.eq(inst.id))
            .one(&state.web_db)
            .await
            .ok()
            .flatten();
        match integration {
            Some(i) => {
                let allowlist = i.allowed_ips.clone().unwrap_or_default();
                if !ip_allowed(client_ip, &allowlist) {
                    warn!(
                        %org_id,
                        integration_id = %i.id,
                        %client_ip,
                        "resolve_github_app_targets: source IP not allowed, skipping integration"
                    );
                    continue;
                }
                integrations.push(i.id);
            }
            None => warn!(
                %org_id,
                "resolve_github_app_targets: org has matching project but no inbound github integration row"
            ),
        }
    }
    integrations
}

#[cfg(test)]
mod tests {
    use super::{event_repo_matches_project, github_full_name, normalize_repo_url};

    #[test]
    fn event_repo_matches_project_is_host_agnostic_on_owner_repo() {
        let event = [
            "https://github.com/NuschtOS/search.nuschtos.de.git".to_string(),
            "git@github.com:NuschtOS/search.nuschtos.de.git".to_string(),
        ];
        for project in [
            "https://github.com/NuschtOS/search.nuschtos.de",
            "https://github.com/nuschtos/search.nuschtos.de.git",
            "git@github.com:NuschtOS/search.nuschtos.de.git",
        ] {
            assert!(event_repo_matches_project(&event, project), "{project}");
        }
    }

    #[test]
    fn event_repo_rejects_a_sibling_repo_in_the_same_org() {
        let event = ["https://github.com/NuschtOS/search.git".to_string()];
        assert!(!event_repo_matches_project(
            &event,
            "https://github.com/NuschtOS/search.nuschtos.de"
        ));
    }

    #[test]
    fn event_repo_empty_urls_match_every_project() {
        assert!(event_repo_matches_project(
            &[],
            "https://github.com/NuschtOS/search"
        ));
    }

    #[test]
    fn event_repo_unparsable_project_never_matches() {
        let event = ["https://github.com/NuschtOS/search".to_string()];
        assert!(!event_repo_matches_project(&event, "not-a-url"));
    }

    #[test]
    fn github_full_name_parses_every_url_form() {
        for url in [
            "https://github.com/NuschtOS/search.git",
            "https://github.com/NuschtOS/search",
            "git+https://github.com/NuschtOS/search",
            "git@github.com:NuschtOS/search.git",
            "github:NuschtOS/search",
            "git+github:NuschtOS/search",
        ] {
            assert_eq!(
                github_full_name(url).as_deref(),
                Some("nuschtos/search"),
                "{url}"
            );
        }
    }

    #[test]
    fn github_full_name_rejects_non_github_hosts() {
        assert_eq!(github_full_name("https://gitlab.com/NuschtOS/search"), None);
        assert_eq!(
            github_full_name("https://gitea.example.com/acme/widgets"),
            None
        );
    }

    #[test]
    fn normalize_strips_dot_git_suffix() {
        assert_eq!(
            normalize_repo_url("https://github.com/owner/repo.git"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn normalize_strips_trailing_slash() {
        assert_eq!(
            normalize_repo_url("https://github.com/owner/repo/"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn normalize_rewrites_ssh_to_https() {
        assert_eq!(
            normalize_repo_url("git@github.com:owner/repo.git"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn normalize_passes_through_canonical_form() {
        assert_eq!(
            normalize_repo_url("https://github.com/owner/repo"),
            "https://github.com/owner/repo"
        );
    }

    #[test]
    fn normalize_collapses_url_variants() {
        let canonical = normalize_repo_url("https://github.com/owner/repo");
        for url in [
            "https://github.com/owner/repo.git",
            "https://github.com/owner/repo/",
            "git@github.com:owner/repo.git",
            "  https://github.com/owner/repo  ",
        ] {
            assert_eq!(normalize_repo_url(url), canonical, "input was {url}");
        }
    }
}
