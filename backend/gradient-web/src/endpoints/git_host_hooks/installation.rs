/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::payloads::GitHubInstallationPayload;
use gradient_ci::parse_owner_repo;
use gradient_db::WebDb;
use gradient_types::*;
use tracing::{info, warn};

pub(super) async fn handle_github_installation(db: &WebDb, body: &[u8]) {
    let payload: GitHubInstallationPayload = match serde_json::from_slice(body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to parse GitHub installation payload");
            return;
        }
    };
    let installation_id = payload.installation.id;

    if payload.action == "deleted" {
        if let Err(e) =
            gradient_db::git_host_connections::delete_by_github_installation_id(db, installation_id)
                .await
        {
            warn!(error = %e, installation_id, "Failed to delete the Git host connection for the installation of the GitHub App");
        }
        return;
    }

    let Some(creator) = gradient_db::access::superusers(db)
        .await
        .unwrap_or_default()
        .into_iter()
        .next()
    else {
        warn!(
            installation_id,
            "No instance administrator for the connection to the Git host for the installation of the GitHub App. Skipping."
        );
        return;
    };
    let account_login = payload.installation.account.login.as_str();
    match gradient_ci::github_app_connection::upsert_github_app_installation_git_host_connection(
        db.inner(),
        installation_id,
        Some(account_login),
        creator.id,
    )
    .await
    {
        Ok(connection) => {
            info!(installation_id, connection = %connection.id, account_login, "Saved a connection to the Git host for an installation of the GitHub App")
        }
        Err(e) => {
            warn!(error = %e, installation_id, "Failed to save the connection to the Git host for the installation of the GitHub App.")
        }
    }
}

pub(crate) fn repo_identity(url: &str) -> Option<String> {
    parse_owner_repo(url).map(|(owner, repo)| format!("{owner}/{repo}").to_ascii_lowercase())
}

/// A connection to a Git host that many tasks share can otherwise fan out to sibling tasks. An empty URL list can match every task.
pub(super) fn event_repo_matches_task(event_repo_urls: &[String], task_repository: &str) -> bool {
    let mut keys = event_repo_urls
        .iter()
        .filter_map(|u| repo_identity(u))
        .peekable();
    if keys.peek().is_none() {
        return true;
    }

    match repo_identity(task_repository) {
        Some(target) => keys.any(|k| k == target),
        None => false,
    }
}

pub(super) fn task_serves_repository(task_repository: &str, owner: &str, repo: &str) -> bool {
    repo_identity(task_repository)
        .is_some_and(|identity| identity == format!("{owner}/{repo}").to_ascii_lowercase())
}

pub(super) async fn resolve_github_app_targets(
    db: &WebDb,
    installation_id: i64,
    client_ip: std::net::IpAddr,
) -> Vec<GitHostConnectionId> {
    use crate::ip_allowlist::is_allowed as ip_allowed;

    let connection = match gradient_db::git_host_connections::by_github_installation_id(
        db,
        installation_id,
    )
    .await
    {
        Ok(Some(connection)) => connection,
        Ok(None) => {
            warn!(
                installation_id,
                "The webhook of the GitHub App received no handling: no connection to the Git host for that installation."
            );
            return Vec::new();
        }
        Err(e) => {
            warn!(error = %e, installation_id, "resolve_github_app_targets: connection lookup failed");
            return Vec::new();
        }
    };
    if !ip_allowed(
        client_ip,
        connection.allowed_ips.as_deref().unwrap_or_default(),
    ) {
        warn!(installation_id, connection = %connection.id, %client_ip, "GitHub App webhook from a source IP that is not on the allowed_ips list for this connection. Ignore this webhook.");
        return Vec::new();
    }
    vec![connection.id]
}

#[cfg(test)]
mod tests {
    use super::event_repo_matches_task;

    #[test]
    fn a_task_url_with_a_flake_directory_matches_its_push() {
        assert!(event_repo_matches_task(
            &["https://github.com/acme/infra.git".into()],
            "https://github.com/acme/infra?dir=nix"
        ));
    }

    #[test]
    fn event_repo_matches_task_is_host_agnostic_on_owner_repo() {
        let event = [
            "https://github.com/NuschtOS/search.nuschtos.de.git".to_string(),
            "git@github.com:NuschtOS/search.nuschtos.de.git".to_string(),
        ];
        for task in [
            "https://github.com/NuschtOS/search.nuschtos.de",
            "https://github.com/nuschtos/search.nuschtos.de.git",
            "git@github.com:NuschtOS/search.nuschtos.de.git",
        ] {
            assert!(event_repo_matches_task(&event, task), "{task}");
        }
    }

    #[test]
    fn event_repo_rejects_a_sibling_repo_in_the_same_project() {
        let event = ["https://github.com/NuschtOS/search.git".to_string()];
        assert!(!event_repo_matches_task(
            &event,
            "https://github.com/NuschtOS/search.nuschtos.de"
        ));
    }

    #[test]
    fn event_repo_empty_urls_match_every_task() {
        assert!(event_repo_matches_task(
            &[],
            "https://github.com/NuschtOS/search"
        ));
    }

    #[test]
    fn event_repo_unparsable_task_never_matches() {
        let event = ["https://github.com/NuschtOS/search".to_string()];
        assert!(!event_repo_matches_task(&event, "not-a-url"));
    }
}
