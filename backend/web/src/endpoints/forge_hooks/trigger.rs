/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation triggering from forge webhooks.

use core::ci::{TriggerError, trigger_evaluation};
use core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use serde::Deserialize;
use std::sync::Arc;
use tracing::{debug, info, warn};

// ── GitHub installation payload ────────────────────────────────────────────

#[derive(Deserialize)]
pub(super) struct GitHubInstallationPayload {
    pub action: String,
    pub installation: GitHubInstallation,
    pub sender: Option<GitHubSender>,
}

#[derive(Deserialize)]
pub(super) struct GitHubInstallation {
    pub id: i64,
    pub account: GitHubAccount,
}

#[derive(Deserialize)]
pub(super) struct GitHubAccount {
    pub login: String,
}

#[derive(Deserialize)]
pub(super) struct GitHubSender {
    pub login: String,
}

// ── GitHub App installation event ──────────────────────────────────────────

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
    if let Ok(orgs) = EOrganization::find()
        .filter(COrganization::GithubInstallationId.eq(installation_id))
        .all(&state.db)
        .await
    {
        for org in orgs {
            let mut active = org.into_active_model();
            active.github_installation_id = Set(None);
            if let Err(e) = active.update(&state.db).await {
                warn!(error = %e, "Failed to clear github_installation_id");
            }
        }
    }
}

async fn store_installation_id(state: &Arc<ServerState>, payload: &GitHubInstallationPayload) {
    let github_login = &payload.installation.account.login;
    if let Ok(Some(org)) = EOrganization::find()
        .filter(COrganization::Name.eq(github_login.as_str()))
        .one(&state.db)
        .await
    {
        let installation_id = payload.installation.id;
        let mut active = org.into_active_model();
        active.github_installation_id = Set(Some(installation_id));
        if let Err(e) = active.update(&state.db).await {
            warn!(error = %e, installation_id, org_name = %github_login, "Failed to store github_installation_id");
        } else {
            info!(installation_id, org_name = %github_login, "GitHub App installed on organization");
        }
    } else {
        let sender_login = payload
            .sender
            .as_ref()
            .map(|s| s.login.as_str())
            .unwrap_or("unknown");
        warn!(
            github_login = %github_login,
            sender = %sender_login,
            installation_id = payload.installation.id,
            "GitHub App installed but no matching Gradient organization found"
        );
    }
}

// ── Project evaluation trigger ─────────────────────────────────────────────

/// Canonicalises a git repository URL so that equivalent forms compare equal.
///
/// Handles:
/// - Trailing whitespace, `/`, and `.git`.
/// - `git+ssh://` / `git+https://` scheme prefixes (flake/fetchGit form) →
///   `ssh://` / `https://`.
/// - SCP-style SSH refs (`user@host:path`) → `ssh://user@host/path`.
/// - `http://` → `https://` so both schemes match.
///
/// TODO: save canonicalised URLs in the DB (on project create/update) so the
/// webhook path can do a cheap indexed lookup instead of scanning every active
/// project and canonicalising on every push.
pub(super) fn canonicalise_repo_url(u: &str) -> String {
    let u = u.trim();
    let u = u.trim_end_matches('/');
    let u = u.trim_end_matches(".git");
    let u = u.strip_prefix("git+").unwrap_or(u);
    // SCP form: `user@host:path` (no `://`) → `ssh://user@host/path`
    let normalised = if !u.contains("://") {
        if let Some((userhost, path)) = u.split_once(':') {
            format!("ssh://{}/{}", userhost, path)
        } else {
            u.to_string()
        }
    } else {
        u.to_string()
    };
    // Treat http and https as equivalent — forges typically redirect one to the other.
    if let Some(rest) = normalised.strip_prefix("http://") {
        format!("https://{}", rest)
    } else {
        normalised
    }
}

/// Finds all active projects whose repository URL matches any of `candidate_urls`
/// and queues an evaluation for each one.
pub(super) async fn trigger_for_repo_urls(
    state: &Arc<ServerState>,
    candidate_urls: &[&str],
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) {
    let normalised_candidates: Vec<String> = candidate_urls
        .iter()
        .map(|u| canonicalise_repo_url(u))
        .collect();

    let projects = match EProject::find()
        .filter(CProject::Active.eq(true))
        .all(&state.db)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "DB error fetching projects for forge webhook");
            return;
        }
    };

    for project in projects {
        let repo_normalised = canonicalise_repo_url(&project.repository);
        if !normalised_candidates.iter().any(|c| c == &repo_normalised) {
            continue;
        }

        match trigger_evaluation(
            &state.db,
            &project,
            commit_hash.clone(),
            commit_message.clone(),
            author_name.clone(),
        )
        .await
        {
            Ok(eval) => {
                info!(
                    project_id = %project.id,
                    evaluation_id = %eval.id,
                    "Forge webhook triggered evaluation"
                );
            }
            Err(TriggerError::AlreadyInProgress) => {
                debug!(project_id = %project.id, "Evaluation already in progress, skipping webhook trigger");
            }
            Err(TriggerError::NoPreviousEvaluation) => {}
            Err(TriggerError::Db(e)) => {
                warn!(error = %e, project_id = %project.id, "DB error triggering evaluation from forge webhook");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::canonicalise_repo_url;

    #[test]
    fn canonicalise_strips_dot_git_and_trailing_slash() {
        assert_eq!(
            canonicalise_repo_url("https://git.example.com/org/repo.git/"),
            "https://git.example.com/org/repo"
        );
    }

    #[test]
    fn canonicalise_strips_git_plus_ssh_prefix() {
        assert_eq!(
            canonicalise_repo_url("git+ssh://git@git.example.com/org/repo.git"),
            "ssh://git@git.example.com/org/repo"
        );
    }

    #[test]
    fn canonicalise_strips_git_plus_https_prefix() {
        assert_eq!(
            canonicalise_repo_url("git+https://git.example.com/org/repo"),
            "https://git.example.com/org/repo"
        );
    }

    #[test]
    fn canonicalise_converts_scp_form_to_ssh_url() {
        assert_eq!(
            canonicalise_repo_url("git@git.example.com:org/repo.git"),
            "ssh://git@git.example.com/org/repo"
        );
    }

    #[test]
    fn canonicalise_upgrades_http_to_https() {
        assert_eq!(
            canonicalise_repo_url("http://git.example.com/org/repo"),
            "https://git.example.com/org/repo"
        );
    }

    #[test]
    fn canonicalise_equates_flake_ssh_and_scp_forms() {
        let scp = canonicalise_repo_url("git@git.example.com:org/repo.git");
        let flake = canonicalise_repo_url("git+ssh://git@git.example.com/org/repo.git");
        let plain_ssh = canonicalise_repo_url("ssh://git@git.example.com/org/repo");
        assert_eq!(scp, flake);
        assert_eq!(flake, plain_ssh);
    }

    #[test]
    fn canonicalise_equates_flake_https_and_plain_https() {
        let a = canonicalise_repo_url("git+https://git.example.com/org/repo.git");
        let b = canonicalise_repo_url("https://git.example.com/org/repo");
        assert_eq!(a, b);
    }

    #[test]
    fn canonicalise_is_idempotent() {
        let once = canonicalise_repo_url("git+ssh://git@git.example.com/org/repo.git/");
        let twice = canonicalise_repo_url(&once);
        assert_eq!(once, twice);
    }
}
