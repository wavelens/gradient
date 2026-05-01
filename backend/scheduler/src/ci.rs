/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ci::{
    CiReport, CiStatus, parse_owner_repo, resolve_outbound_reporter_for_project,
};
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
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

    let ep_rows = match EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation_id))
        .all(&state.db)
        .await
    {
        Ok(rows) => rows,
        Err(e) => {
            error!(error = %e, %evaluation_id, "Failed to load entry_points for CI reporting");
            return;
        }
    };
    if ep_rows.is_empty() {
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

    let reporter = resolve_outbound_reporter_for_project(&state, project_id).await;

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
            warn!(
                repository_url,
                "Could not parse owner/repo for CI reporting"
            );
            return;
        }
    };

    let org_name = match EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
    {
        Ok(Some(org)) => Some(org.name),
        _ => None,
    };

    let details_url = org_name.map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.cli.frontend_url, org, evaluation_id
        )
    });

    for ep in ep_rows {
        let report = CiReport {
            owner: owner.clone(),
            repo: repo.clone(),
            sha: sha.clone(),
            context: format!("gradient/{}", ep.eval),
            status: status.clone(),
            description: None,
            details_url: details_url.clone(),
            existing_check_id: ep.repo_check_id,
        };

        match reporter.report(&report).await {
            Ok(Some(new_id)) => {
                let mut a = ep.clone().into_active_model();
                a.repo_check_id = Set(Some(new_id));
                if let Err(e) = a.update(&state.db).await {
                    warn!(error = %e, eval = %ep.eval, "Failed to persist entry_point check_run id");
                }
            }
            Ok(None) => {}
            Err(e) => {
                warn!(error = format!("{e:#}"), eval = %ep.eval, "CI status report failed");
            }
        }
    }
    let _ = entry_points; // signature kept for backwards-compatibility with callers
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

    let reporter = resolve_outbound_reporter_for_project(&state, project_id).await;

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
            warn!(
                repository_url,
                "Could not parse owner/repo for CI evaluation report"
            );
            return;
        }
    };

    let org_name = match EOrganization::find_by_id(project.organization)
        .one(&state.db)
        .await
    {
        Ok(Some(org)) => Some(org.name),
        _ => None,
    };

    let details_url = org_name.map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.cli.frontend_url, org, evaluation_id
        )
    });

    let evaluation = match EEvaluation::find_by_id(evaluation_id).one(&state.db).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            warn!(%evaluation_id, "Evaluation not found for CI evaluation report");
            return;
        }
        Err(e) => {
            error!(error = %e, %evaluation_id, "Failed to query evaluation for CI report");
            return;
        }
    };

    let report = CiReport {
        owner,
        repo,
        sha,
        context: "Gradient Evaluation".to_string(),
        status,
        description: None,
        details_url,
        existing_check_id: evaluation.repo_check_id,
    };

    match reporter.report(&report).await {
        Ok(Some(new_id)) => {
            let mut a = evaluation.into_active_model();
            a.repo_check_id = Set(Some(new_id));
            if let Err(e) = a.update(&state.db).await {
                warn!(error = %e, %evaluation_id, "Failed to persist evaluation check_run id");
            }
        }
        Ok(None) => {}
        Err(e) => warn!(error = format!("{e:#}"), "CI evaluation status report failed"),
    }
}
