/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use gradient_core::ci::{CiReport, CiStatus, parse_owner_repo, reporter_for_project};
use gradient_core::ci::decrypt_webhook_secret;
use gradient_core::types::input::vec_to_hex;
use gradient_core::db::update_build_status;
use gradient_core::types::*;
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter};
use std::collections::{HashSet, VecDeque};
use std::sync::Arc;
use tracing::{error, warn};
use uuid::Uuid;

/// Propagates a status change through the dependent build graph.
///
/// Dependents of a `Failed` build are set to `Aborted` (they didn't fail themselves).
/// After all propagation is complete the original build is updated and the evaluation
/// status is re-checked.
pub(super) async fn update_build_status_recursivly(
    state: Arc<ServerState>,
    build: MBuild,
    status: BuildStatus,
) -> MBuild {
    let evaluation_id = build.evaluation;
    let mut queue = VecDeque::new();
    let mut processed = HashSet::new();
    // Each queued entry is (build_id, derivation_id).
    queue.push_back((build.id, build.derivation));

    while let Some((current_build_id, current_derivation_id)) = queue.pop_front() {
        if !processed.insert(current_build_id) {
            continue;
        }

        // Walk reverse derivation_dependency edges: which derivations
        // depend on `current_derivation_id`?
        let reverse_edges = match EDerivationDependency::find()
            .filter(CDerivationDependency::Dependency.eq(current_derivation_id))
            .all(&state.db)
            .await
        {
            Ok(edges) => edges,
            Err(e) => {
                error!(error = %e, %current_derivation_id, "Failed to query reverse derivation_dependency");
                continue;
            }
        };

        if reverse_edges.is_empty() {
            continue;
        }

        // Map back to builds of the same evaluation.
        let dependent_derivation_ids: Vec<Uuid> =
            reverse_edges.into_iter().map(|e| e.derivation).collect();
        let mut dep_build_cond = Condition::any();
        for did in &dependent_derivation_ids {
            dep_build_cond = dep_build_cond.add(CBuild::Derivation.eq(*did));
        }

        let status_condition = if status == BuildStatus::Aborted
            || status == BuildStatus::DependencyFailed
            || status == BuildStatus::Failed
        {
            Condition::any()
                .add(CBuild::Status.eq(BuildStatus::Created))
                .add(CBuild::Status.eq(BuildStatus::Queued))
                .add(CBuild::Status.eq(BuildStatus::Building))
        } else {
            Condition::all().add(CBuild::Status.ne(status.clone()))
        };

        let dependent_builds = match EBuild::find()
            .filter(CBuild::Evaluation.eq(evaluation_id))
            .filter(dep_build_cond)
            .filter(status_condition)
            .all(&state.db)
            .await
        {
            Ok(builds) => builds,
            Err(e) => {
                error!(error = %e, "Failed to query dependent builds for update");
                continue;
            }
        };

        // Update dependent builds and add them to the queue for further processing.
        // Dependents of a failed build get DependencyFailed (they didn't fail themselves).
        let propagated_status =
            if status == BuildStatus::Failed || status == BuildStatus::DependencyFailed {
                BuildStatus::DependencyFailed
            } else {
                status.clone()
            };
        for dependent_build in dependent_builds {
            let dep_id = dependent_build.id;
            let dep_drv = dependent_build.derivation;
            update_build_status(
                Arc::clone(&state),
                dependent_build,
                propagated_status.clone(),
            )
            .await;
            queue.push_back((dep_id, dep_drv));
        }
    }

    // Finally update the original build with the actual status.
    let build = update_build_status(Arc::clone(&state), build, status.clone()).await;
    check_evaluation_status(state, build.evaluation).await;

    build
}

/// Determines whether an evaluation is fully finished and updates its status accordingly.
///
/// Called after each build status change to detect when all builds have reached a terminal state.
pub(super) async fn check_evaluation_status(state: Arc<ServerState>, evaluation_id: Uuid) {
    let evaluation = match EEvaluation::find_by_id(evaluation_id).one(&state.db).await {
        Ok(Some(eval)) => eval,
        Ok(None) => {
            error!(evaluation_id = %evaluation_id, "Evaluation not found for status check");
            return;
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation_id, "Failed to query evaluation for status check");
            return;
        }
    };

    let builds = match EBuild::find()
        .filter(CBuild::Evaluation.eq(evaluation_id))
        .all(&state.db)
        .await
    {
        Ok(builds) => builds,
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation_id, "Failed to query builds for evaluation status check");
            return;
        }
    };

    let statuses = builds
        .into_iter()
        .map(|b| b.status)
        .collect::<Vec<BuildStatus>>();

    let eval_status = match eval_terminal_status(&statuses) {
        Some(s) => s,
        None => return,
    };

    report_ci_completion(Arc::clone(&state), &evaluation, eval_status.clone()).await;
    gradient_core::db::update_evaluation_status(state, evaluation, eval_status).await;
}

/// Reports a CI status for each entry point of a completed/failed evaluation.
async fn report_ci_completion(
    state: Arc<ServerState>,
    evaluation: &MEvaluation,
    eval_status: EvaluationStatus,
) {
    let project_id = match evaluation.project {
        Some(id) => id,
        None => return,
    };

    let project = match EProject::find_by_id(project_id).one(&state.db).await {
        Ok(Some(p)) => p,
        _ => return,
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

    let commit = match ECommit::find_by_id(evaluation.commit).one(&state.db).await {
        Ok(Some(c)) => c,
        _ => return,
    };

    let sha = vec_to_hex(&commit.hash);

    let (owner, repo) = match parse_owner_repo(&evaluation.repository) {
        Some(pair) => pair,
        None => {
            warn!(repository = %evaluation.repository, "Could not parse owner/repo for CI completion report");
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
            state.cli.frontend_url, org, evaluation.id
        )
    });

    let entry_points = match EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation.id))
        .all(&state.db)
        .await
    {
        Ok(eps) => eps,
        Err(e) => {
            error!(error = %e, "Failed to query entry points for CI completion report");
            return;
        }
    };

    // Report the top-level "gradient" check (overall evaluation result).
    let overall_ci_status = match eval_status {
        EvaluationStatus::Completed => CiStatus::Success,
        EvaluationStatus::Failed => CiStatus::Failure,
        _ => CiStatus::Error,
    };
    let overall_report = CiReport {
        owner: owner.clone(),
        repo: repo.clone(),
        sha: sha.clone(),
        context: "gradient".to_string(),
        status: overall_ci_status,
        description: None,
        details_url: details_url.clone(),
    };
    if let Err(e) = reporter.report(&overall_report).await {
        warn!(error = %e, "CI overall completion report failed");
    }

    for ep in &entry_points {
        // Determine per-entry-point status from its root build.
        let ci_status = match EBuild::find_by_id(ep.build).one(&state.db).await {
            Ok(Some(build)) => match build.status {
                BuildStatus::Completed | BuildStatus::Substituted => CiStatus::Success,
                BuildStatus::Failed => CiStatus::Failure,
                _ => match eval_status {
                    EvaluationStatus::Completed => CiStatus::Success,
                    EvaluationStatus::Failed => CiStatus::Failure,
                    _ => CiStatus::Error,
                },
            },
            _ => CiStatus::Error,
        };

        let report = CiReport {
            owner: owner.clone(),
            repo: repo.clone(),
            sha: sha.clone(),
            context: format!("gradient/{}", ep.eval),
            status: ci_status,
            description: None,
            details_url: details_url.clone(),
        };

        if let Err(e) = reporter.report(&report).await {
            warn!(error = %e, eval = %ep.eval, "CI completion report failed");
        }
    }
}

/// Pure function that determines the terminal `EvaluationStatus` from a slice
/// of build statuses, or returns `None` if builds are still in progress.
fn eval_terminal_status(statuses: &[BuildStatus]) -> Option<EvaluationStatus> {
    let in_progress = statuses.iter().any(|s| {
        matches!(s, BuildStatus::Queued | BuildStatus::Created | BuildStatus::Building)
    });

    if statuses.iter().all(|s| matches!(s, BuildStatus::Completed | BuildStatus::Substituted)) {
        Some(EvaluationStatus::Completed)
    } else if !in_progress && statuses.contains(&BuildStatus::Failed) {
        Some(EvaluationStatus::Failed)
    } else if !in_progress
        && (statuses.contains(&BuildStatus::Aborted)
            || statuses.contains(&BuildStatus::DependencyFailed))
    {
        Some(EvaluationStatus::Aborted)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eval_status_all_completed() {
        let statuses = vec![BuildStatus::Completed, BuildStatus::Substituted, BuildStatus::Completed];
        assert_eq!(eval_terminal_status(&statuses), Some(EvaluationStatus::Completed));
    }

    #[test]
    fn eval_status_failed_no_active() {
        let statuses = vec![BuildStatus::Failed, BuildStatus::Completed];
        assert_eq!(eval_terminal_status(&statuses), Some(EvaluationStatus::Failed));
    }

    #[test]
    fn eval_status_active_builds_none() {
        let statuses = vec![BuildStatus::Queued, BuildStatus::Completed];
        assert_eq!(eval_terminal_status(&statuses), None);
    }

    #[test]
    fn eval_status_aborted_no_active() {
        let statuses = vec![BuildStatus::Aborted, BuildStatus::DependencyFailed];
        assert_eq!(eval_terminal_status(&statuses), Some(EvaluationStatus::Aborted));
    }

    #[test]
    fn eval_status_empty_builds() {
        // All vacuously Completed/Substituted (empty iter)
        assert_eq!(eval_terminal_status(&[]), Some(EvaluationStatus::Completed));
    }
}
