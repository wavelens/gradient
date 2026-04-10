/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ci::{CiReport, CiStatus, decrypt_webhook_secret, parse_owner_repo, reporter_for_project};
use gradient_core::types::*;
use sea_orm::EntityTrait;
use std::sync::Arc;
use tracing::{error, warn};
use uuid::Uuid;

/// Fetches the project and commit for the evaluation, then fires one CI status
/// report per entry point using the project's configured reporter.
///
/// Failures are logged and swallowed — CI reporting is best-effort.
pub async fn report_ci_for_entry_points(
    state: Arc<ServerState>,
    project_id: Uuid,
    commit_id: Uuid,
    repository_url: &str,
    evaluation_id: Uuid,
    entry_points: &[(Uuid, String)],
    status: CiStatus,
) {
    if entry_points.is_empty() {
        return;
    }

    let project = match EProject::find_by_id(project_id).one(&state.db).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            warn!(%project_id, "Project not found for CI reporting");
            return;
        }
        Err(e) => {
            error!(error = %e, %project_id, "Failed to query project for CI reporting");
            return;
        }
    };

    let decrypted_token = project.ci_reporter_token.as_deref().and_then(|enc| {
        match decrypt_webhook_secret(&state.cli.crypt_secret_file, enc) {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(error = %e, "Failed to decrypt CI token, skipping CI reporting");
                None
            }
        }
    });

    let reporter = reporter_for_project(
        project.ci_reporter_type.as_deref(),
        project.ci_reporter_url.as_deref(),
        decrypted_token.as_deref(),
    );

    let commit = match ECommit::find_by_id(commit_id).one(&state.db).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            warn!(%commit_id, "Commit not found for CI reporting");
            return;
        }
        Err(e) => {
            error!(error = %e, %commit_id, "Failed to query commit for CI reporting");
            return;
        }
    };

    let sha = gradient_core::types::input::vec_to_hex(&commit.hash);

    let (owner, repo) = match parse_owner_repo(repository_url) {
        Some(pair) => pair,
        None => {
            warn!(repository_url, "Could not parse owner/repo for CI reporting");
            return;
        }
    };

    let org_name = match EOrganization::find_by_id(project.organization).one(&state.db).await {
        Ok(Some(org)) => Some(org.name),
        _ => None,
    };

    let details_url = org_name.map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.cli.frontend_url, org, evaluation_id
        )
    });

    for (_build_id, eval) in entry_points {
        let report = CiReport {
            owner: owner.clone(),
            repo: repo.clone(),
            sha: sha.clone(),
            context: format!("gradient/{}", eval),
            status: status.clone(),
            description: None,
            details_url: details_url.clone(),
        };

        if let Err(e) = reporter.report(&report).await {
            warn!(error = %e, eval, "CI status report failed");
        }
    }
}

/// Reports a single `"gradient"` top-level CI status for the whole evaluation.
///
/// - **Running** when evaluation starts (before nix eval).
/// - **Failure** if nix eval itself fails.
/// - **Success / Failure / Error** when all builds finish (reported from builder).
///
/// Links always point to the evaluation log page.
pub async fn report_ci_for_evaluation(
    state: Arc<ServerState>,
    project_id: Uuid,
    commit_id: Uuid,
    repository_url: &str,
    evaluation_id: Uuid,
    status: CiStatus,
) {
    let project = match EProject::find_by_id(project_id).one(&state.db).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            warn!(%project_id, "Project not found for CI evaluation report");
            return;
        }
        Err(e) => {
            error!(error = %e, %project_id, "Failed to query project for CI evaluation report");
            return;
        }
    };

    let decrypted_token = project.ci_reporter_token.as_deref().and_then(|enc| {
        match decrypt_webhook_secret(&state.cli.crypt_secret_file, enc) {
            Ok(t) => Some(t),
            Err(e) => {
                warn!(error = %e, "Failed to decrypt CI token, skipping CI evaluation report");
                None
            }
        }
    });

    let reporter = reporter_for_project(
        project.ci_reporter_type.as_deref(),
        project.ci_reporter_url.as_deref(),
        decrypted_token.as_deref(),
    );

    let commit = match ECommit::find_by_id(commit_id).one(&state.db).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            warn!(%commit_id, "Commit not found for CI evaluation report");
            return;
        }
        Err(e) => {
            error!(error = %e, %commit_id, "Failed to query commit for CI evaluation report");
            return;
        }
    };

    let sha = gradient_core::types::input::vec_to_hex(&commit.hash);

    let (owner, repo) = match parse_owner_repo(repository_url) {
        Some(pair) => pair,
        None => {
            warn!(repository_url, "Could not parse owner/repo for CI evaluation report");
            return;
        }
    };

    let org_name = match EOrganization::find_by_id(project.organization).one(&state.db).await {
        Ok(Some(org)) => Some(org.name),
        _ => None,
    };

    let details_url = org_name.map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.cli.frontend_url, org, evaluation_id
        )
    });

    let report = CiReport {
        owner,
        repo,
        sha,
        context: "gradient".to_string(),
        status,
        description: None,
        details_url,
    };

    if let Err(e) = reporter.report(&report).await {
        warn!(error = %e, "CI evaluation status report failed");
    }
}
