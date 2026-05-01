/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! High-level helpers that turn an evaluation/build outcome into a CI status
//! report and fire it through the project's configured outbound reporter.

use crate::ci::{CiReport, CiStatus, parse_owner_repo, resolve_outbound_reporter_for_project};
use crate::types::input::vec_to_hex;
use crate::types::*;
use entity::evaluation::EvaluationStatus;
use sea_orm::EntityTrait;
use std::sync::Arc;
use tracing::{error, warn};

/// Maps an [`EvaluationStatus`] to the [`CiStatus`] reported to external forges.
///
/// Returns `None` for non-terminal/intermediate states that do not produce a
/// CI report from this helper (the per-job handlers report `Running` directly
/// when an evaluation starts).
pub fn ci_status_for_evaluation(status: &EvaluationStatus) -> Option<CiStatus> {
    match status {
        EvaluationStatus::Completed => Some(CiStatus::Success),
        EvaluationStatus::Failed => Some(CiStatus::Failure),
        EvaluationStatus::Aborted => Some(CiStatus::Error),
        EvaluationStatus::Queued
        | EvaluationStatus::Fetching
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation
        | EvaluationStatus::Building
        | EvaluationStatus::Waiting => None,
    }
}

/// Reports a single top-level `"gradient"` CI status for the whole evaluation.
///
/// No-ops when the evaluation has no associated project (direct builds), when
/// the project is not found, or when the configured forge does not support
/// outbound status reporting.
pub async fn report_evaluation_ci(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: CiStatus,
) {
    let Some(project_id) = evaluation.project else {
        return;
    };

    let project = match EProject::find_by_id(project_id).one(&state.db).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            warn!(%project_id, "Project not found for evaluation CI report");
            return;
        }
        Err(e) => {
            error!(error = %e, %project_id, "Failed to query project for evaluation CI report");
            return;
        }
    };

    let reporter = resolve_outbound_reporter_for_project(&state, project_id).await;

    let commit = match ECommit::find_by_id(evaluation.commit).one(&state.db).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            warn!(commit_id = %evaluation.commit, "Commit not found for evaluation CI report");
            return;
        }
        Err(e) => {
            error!(error = %e, commit_id = %evaluation.commit, "Failed to query commit for evaluation CI report");
            return;
        }
    };

    let sha = vec_to_hex(&commit.hash);

    let (owner, repo) = match parse_owner_repo(&evaluation.repository) {
        Some(pair) => pair,
        None => {
            warn!(
                repository_url = %evaluation.repository,
                "Could not parse owner/repo for evaluation CI report"
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
            state.cli.frontend_url, org, evaluation.id
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
        warn!(error = %e, evaluation_id = %evaluation.id, "Evaluation CI status report failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_terminal_states() {
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Completed),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Failed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_evaluation(&EvaluationStatus::Aborted),
            Some(CiStatus::Error)
        );
    }

    #[test]
    fn skips_intermediate_states() {
        for s in [
            EvaluationStatus::Queued,
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
            EvaluationStatus::Waiting,
        ] {
            assert_eq!(ci_status_for_evaluation(&s), None);
        }
    }
}
