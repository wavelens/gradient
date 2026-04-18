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

/// Finds all active projects whose repository URL matches any of `candidate_urls`
/// and queues an evaluation for each one.
pub(super) async fn trigger_for_repo_urls(
    state: &Arc<ServerState>,
    candidate_urls: &[&str],
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) {
    let normalise = |u: &str| u.trim_end_matches(".git").to_string();
    let normalised_candidates: Vec<String> = candidate_urls.iter().map(|u| normalise(u)).collect();

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
        let repo_normalised = normalise(&project.repository);
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
