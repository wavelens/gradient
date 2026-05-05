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
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::sync::Arc;
use tracing::{error, warn};

/// `"{org}/{project}"` when both are known, falling back to `"{project}"` when
/// the organization lookup turned up nothing. Used as the scope segment of
/// every CI check name so multiple Gradient projects reporting to the same
/// repository remain distinguishable.
pub fn format_check_scope(org_name: Option<&str>, project_name: &str) -> String {
    match org_name {
        Some(org) => format!("{}/{}", org, project_name),
        None => project_name.to_string(),
    }
}

/// CI check name for the per-evaluation roll-up status.
pub fn evaluation_check_context(scope: &str) -> String {
    format!("Gradient Evaluation {}", scope)
}

/// CI check name for a single entry-point build under an evaluation.
pub fn build_check_context(scope: &str, entry_point: &str) -> String {
    format!("Gradient Build {}: {}", scope, entry_point)
}

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

/// Maps a [`BuildStatus`] to the [`CiStatus`] reported per-entry-point.
///
/// Returns `None` for non-terminal states; the per-eval-name `Pending` is
/// reported once at evaluation time.
pub fn ci_status_for_build(status: &BuildStatus) -> Option<CiStatus> {
    match status {
        BuildStatus::Building => Some(CiStatus::Running),
        BuildStatus::Completed | BuildStatus::Substituted => Some(CiStatus::Success),
        BuildStatus::Failed | BuildStatus::DependencyFailed => Some(CiStatus::Failure),
        BuildStatus::Aborted => Some(CiStatus::Error),
        BuildStatus::Created | BuildStatus::Queued => None,
    }
}

/// Reports per-entry-point CI status for a finished build.
///
/// One `gradient/<eval-name>` status is sent per `entry_point` row pointing at
/// this build (a single derivation can be exposed under multiple flake attrs).
/// No-ops when the build has no entry-point rows (intermediate builds), the
/// evaluation has no project (direct builds), or owner/repo can't be parsed.
pub async fn report_build_ci(state: Arc<ServerState>, build: MBuild, status: CiStatus) {
    let entry_points = match EEntryPoint::find()
        .filter(CEntryPoint::Build.eq(build.id))
        .all(&state.worker_db)
        .await
    {
        Ok(eps) => eps,
        Err(e) => {
            error!(error = %e, build_id = %build.id, "Failed to query entry_points for build CI report");
            return;
        }
    };
    if entry_points.is_empty() {
        return;
    }

    let evaluation = match EEvaluation::find_by_id(build.evaluation)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(e)) => e,
        Ok(None) => {
            warn!(evaluation_id = %build.evaluation, "Evaluation missing for build CI report");
            return;
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %build.evaluation, "Failed to query evaluation for build CI report");
            return;
        }
    };

    let Some(project_id) = evaluation.project else {
        return;
    };

    let project = match EProject::find_by_id(project_id).one(&state.worker_db).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            warn!(%project_id, "Project missing for build CI report");
            return;
        }
        Err(e) => {
            error!(error = %e, %project_id, "Failed to query project for build CI report");
            return;
        }
    };

    let commit = match ECommit::find_by_id(evaluation.commit)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(c)) => c,
        Ok(None) => {
            warn!(commit_id = %evaluation.commit, "Commit missing for build CI report");
            return;
        }
        Err(e) => {
            error!(error = %e, commit_id = %evaluation.commit, "Failed to query commit for build CI report");
            return;
        }
    };

    let sha = vec_to_hex(&commit.hash);

    let (owner, repo) = match parse_owner_repo(&evaluation.repository) {
        Some(pair) => pair,
        None => {
            warn!(repository_url = %evaluation.repository, "Could not parse owner/repo for build CI report");
            return;
        }
    };

    let reporter = resolve_outbound_reporter_for_project(&state, project_id).await;

    let org_name = match EOrganization::find_by_id(project.organization)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(o)) => Some(o.name),
        _ => None,
    };
    let scope = format_check_scope(org_name.as_deref(), &project.name);
    let details_url = org_name.as_ref().map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.config.server.frontend_url, org, evaluation.id
        )
    });

    for ep in entry_points {
        let report = CiReport {
            owner: owner.clone(),
            repo: repo.clone(),
            sha: sha.clone(),
            context: build_check_context(&scope, &ep.eval),
            status: status.clone(),
            description: None,
            details_url: details_url.clone(),
            existing_check_id: ep.repo_check_id,
        };
        match reporter.report(&report).await {
            Ok(Some(new_id)) => {
                let mut a = ep.clone().into_active_model();
                a.repo_check_id = Set(Some(new_id));
                if let Err(e) = a.update(&state.worker_db).await {
                    warn!(error = %e, eval = %ep.eval, "Failed to persist entry_point check_run id");
                }
            }
            Ok(None) => {}
            Err(e) => warn!(
                error = format!("{e:#}"),
                eval = %ep.eval,
                build_id = %build.id,
                "Build CI status report failed"
            ),
        }
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

    let project = match EProject::find_by_id(project_id).one(&state.worker_db).await {
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

    let commit = match ECommit::find_by_id(evaluation.commit)
        .one(&state.worker_db)
        .await
    {
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
        .one(&state.worker_db)
        .await
    {
        Ok(Some(org)) => Some(org.name),
        _ => None,
    };

    let scope = format_check_scope(org_name.as_deref(), &project.name);

    let details_url = org_name.as_ref().map(|org| {
        format!(
            "{}/organization/{}/log/{}",
            state.config.server.frontend_url, org, evaluation.id
        )
    });

    let report = CiReport {
        owner,
        repo,
        sha,
        context: evaluation_check_context(&scope),
        status,
        description: None,
        details_url,
        existing_check_id: evaluation.repo_check_id,
    };

    match reporter.report(&report).await {
        Ok(Some(new_id)) => {
            let mut a = evaluation.clone().into_active_model();
            a.repo_check_id = Set(Some(new_id));
            if let Err(e) = a.update(&state.worker_db).await {
                warn!(error = %e, evaluation_id = %evaluation.id, "Failed to persist evaluation check_run id");
            }
        }
        Ok(None) => {}
        Err(e) => {
            warn!(error = format!("{e:#}"), evaluation_id = %evaluation.id, "Evaluation CI status report failed")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_scope_with_org() {
        assert_eq!(
            format_check_scope(Some("wavelens"), "my-project"),
            "wavelens/my-project"
        );
    }

    #[test]
    fn check_scope_without_org_falls_back_to_project() {
        assert_eq!(format_check_scope(None, "my-project"), "my-project");
    }

    #[test]
    fn evaluation_context_format() {
        assert_eq!(
            evaluation_check_context("wavelens/my-project"),
            "Gradient Evaluation wavelens/my-project"
        );
    }

    #[test]
    fn build_context_format() {
        assert_eq!(
            build_check_context("wavelens/my-project", "my-package"),
            "Gradient Build wavelens/my-project: my-package"
        );
    }

    #[test]
    fn build_context_falls_back_when_org_missing() {
        let scope = format_check_scope(None, "solo-project");
        assert_eq!(
            build_check_context(&scope, "pkg"),
            "Gradient Build solo-project: pkg"
        );
    }

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
    fn maps_build_terminal_states() {
        assert_eq!(
            ci_status_for_build(&BuildStatus::Completed),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Substituted),
            Some(CiStatus::Success)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Failed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::DependencyFailed),
            Some(CiStatus::Failure)
        );
        assert_eq!(
            ci_status_for_build(&BuildStatus::Aborted),
            Some(CiStatus::Error)
        );
    }

    #[test]
    fn skips_intermediate_build_states() {
        for s in [BuildStatus::Created, BuildStatus::Queued] {
            assert_eq!(ci_status_for_build(&s), None);
        }
    }

    #[test]
    fn maps_building_to_running() {
        assert_eq!(
            ci_status_for_build(&BuildStatus::Building),
            Some(CiStatus::Running)
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
