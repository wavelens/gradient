/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ci::{
    APPROVAL_ACTION_ID, CiReport, CiStatus, RequestedAction, build_check_context,
    ci_status_for_build, evaluation_check_context, format_check_scope, parse_owner_repo,
    resolve_outbound_reporter_for_project,
};
use gradient_core::types::waiting_reason::WaitingReason;
use gradient_core::types::*;
use sea_orm::ActiveValue::Set;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{error, warn};

/// Fetches the project and commit for the evaluation, then fires one CI status
/// report per entry point using the project's configured reporter.
///
/// Failures are logged and swallowed - CI reporting is best-effort.
pub async fn report_ci_for_entry_points(
    state: Arc<ServerState>,
    project_id: ProjectId,
    commit_id: CommitId,
    repository_url: &str,
    evaluation_id: EvaluationId,
    entry_points: &[(BuildId, String)],
    status: CiStatus,
) {
    if entry_points.is_empty() {
        return;
    }

    let ep_rows = match EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(evaluation_id))
        .all(&state.worker_db)
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

    let project = match EProject::find_by_id(project_id).one(&state.worker_db).await {
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

    let commit = match ECommit::find_by_id(commit_id).one(&state.worker_db).await {
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
            state.config.server.frontend_url, org, evaluation_id
        )
    });

    // Pre-fetch the build status of each entry point so that builds which are
    // already in a terminal state (notably `Substituted` - set at insert-time
    // and never routed through `update_build_status`) report a terminal CI
    // status on first POST instead of getting stuck at the initial `Pending`.
    let build_ids: Vec<BuildId> = ep_rows.iter().map(|ep| ep.build).collect();
    let build_statuses: HashMap<BuildId, entity::build::BuildStatus> = match EBuild::find()
        .filter(CBuild::Id.is_in(build_ids))
        .all(&state.worker_db)
        .await
    {
        Ok(rows) => rows.into_iter().map(|b| (b.id, b.status)).collect(),
        Err(e) => {
            warn!(error = %e, "Failed to load build statuses for entry-point CI report; defaulting to passed status");
            HashMap::new()
        }
    };

    for ep in ep_rows {
        let initial_status = build_statuses
            .get(&ep.build)
            .and_then(ci_status_for_build)
            .unwrap_or_else(|| status.clone());
        let report = CiReport {
            owner: owner.clone(),
            repo: repo.clone(),
            sha: sha.clone(),
            context: build_check_context(&scope, &ep.eval),
            status: initial_status,
            description: None,
            details_url: details_url.clone(),
            existing_check_id: ep.repo_check_id,
            requested_actions: Vec::new(),
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
            Err(e) => {
                warn!(error = format!("{e:#}"), eval = %ep.eval, "CI status report failed");
            }
        }
    }
    let _ = entry_points; // signature kept for backwards-compatibility with callers
}

/// Spawn a best-effort `Pending` top-level CI report for a freshly-queued
/// evaluation.
///
/// Called from every site that creates a `Queued` evaluation via
/// `apply_trigger` (scheduler trigger dispatch, manual API trigger, forge
/// webhook fan-out). The report goes out as soon as the evaluation row exists
/// - before any worker has picked the eval up - so the commit shows a pending
///   check immediately.
///
/// No-op when the evaluation has no project: direct builds and restart flows
/// don't currently report CI status.
pub fn spawn_pending_ci_for_eval(state: Arc<ServerState>, eval: &MEvaluation) {
    let Some(project_id) = eval.project else {
        return;
    };
    let repo = eval.repository.clone();
    let commit_id = eval.commit;
    let evaluation_id = eval.id;

    let approval_info = eval
        .waiting_reason
        .as_ref()
        .and_then(WaitingReason::from_json)
        .and_then(|r| match r {
            WaitingReason::Approval {
                pr_number,
                pr_author,
            } => Some((pr_number, pr_author)),
            _ => None,
        });

    let s = Arc::clone(&state);
    state.shutdown.spawn(async move {
        if let Some((pr_number, pr_author)) = approval_info {
            report_awaiting_approval_ci(
                s,
                project_id,
                commit_id,
                &repo,
                evaluation_id,
                pr_number,
                &pr_author,
            )
            .await;
        } else {
            report_ci_for_evaluation(
                s,
                project_id,
                commit_id,
                &repo,
                evaluation_id,
                CiStatus::Pending,
            )
            .await;
        }
    });
}

/// Posts the per-evaluation CI check in `ActionRequired` state with an
/// "Approve and run" button (GitHub Apps) or a pending status whose
/// description tells the maintainer to comment `/ci run` (other forges).
///
/// Mirrors [`report_ci_for_evaluation`] but with approval-specific framing.
pub async fn report_awaiting_approval_ci(
    state: Arc<ServerState>,
    project_id: ProjectId,
    commit_id: CommitId,
    repository_url: &str,
    evaluation_id: EvaluationId,
    pr_number: u64,
    pr_author: &str,
) {
    let project = match EProject::find_by_id(project_id).one(&state.worker_db).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            warn!(%project_id, "Project not found for approval-gate CI report");
            return;
        }
        Err(e) => {
            error!(error = %e, %project_id, "Failed to query project for approval-gate CI report");
            return;
        }
    };

    let reporter = resolve_outbound_reporter_for_project(&state, project_id).await;

    let commit = match ECommit::find_by_id(commit_id).one(&state.worker_db).await {
        Ok(Some(c)) => c,
        Ok(None) => {
            warn!(%commit_id, "Commit not found for approval-gate CI report");
            return;
        }
        Err(e) => {
            error!(error = %e, %commit_id, "Failed to query commit for approval-gate CI report");
            return;
        }
    };

    let sha = gradient_core::types::input::vec_to_hex(&commit.hash);

    let (owner, repo) = match parse_owner_repo(repository_url) {
        Some(pair) => pair,
        None => {
            warn!(
                repository_url,
                "Could not parse owner/repo for approval-gate CI report"
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
            state.config.server.frontend_url, org, evaluation_id
        )
    });

    let description = format!(
        "Awaiting maintainer approval for PR #{} by {} - click \"Approve and run\" or comment /ci run.",
        pr_number, pr_author
    );

    let evaluation = match EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await
    {
        Ok(Some(e)) => e,
        Ok(None) => {
            warn!(%evaluation_id, "Evaluation not found for approval-gate CI report");
            return;
        }
        Err(e) => {
            error!(error = %e, %evaluation_id, "Failed to query evaluation for approval-gate CI report");
            return;
        }
    };

    let report = CiReport {
        owner,
        repo,
        sha,
        context: evaluation_check_context(&scope),
        status: CiStatus::ActionRequired,
        description: Some(description),
        details_url,
        existing_check_id: evaluation.repo_check_id,
        requested_actions: vec![RequestedAction {
            identifier: APPROVAL_ACTION_ID.to_string(),
            label: "Approve and run".to_string(),
            description: "Run CI for this PR from an external contributor.".to_string(),
        }],
    };

    match reporter.report(&report).await {
        Ok(Some(new_id)) => {
            let mut a = evaluation.into_active_model();
            a.repo_check_id = Set(Some(new_id));
            if let Err(e) = a.update(&state.worker_db).await {
                warn!(error = %e, %evaluation_id, "Failed to persist approval-gate check_run id");
            }
        }
        Ok(None) => {}
        Err(e) => {
            warn!(error = format!("{e:#}"), %evaluation_id, "Approval-gate CI report failed");
        }
    }
}

/// Reports a single `"gradient"` top-level CI status for the whole evaluation.
///
/// - **Pending** when the evaluation is queued (before any worker picks it up).
/// - **Running** when evaluation starts (before nix eval).
/// - **Failure** if nix eval itself fails.
/// - **Success / Failure / Error** when all builds finish (reported from builder).
///
/// Links always point to the evaluation log page.
pub async fn report_ci_for_evaluation(
    state: Arc<ServerState>,
    project_id: ProjectId,
    commit_id: CommitId,
    repository_url: &str,
    evaluation_id: EvaluationId,
    status: CiStatus,
) {
    let project = match EProject::find_by_id(project_id).one(&state.worker_db).await {
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

    let commit = match ECommit::find_by_id(commit_id).one(&state.worker_db).await {
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
            state.config.server.frontend_url, org, evaluation_id
        )
    });

    let evaluation = match EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await
    {
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
        context: evaluation_check_context(&scope),
        status,
        description: None,
        details_url,
        existing_check_id: evaluation.repo_check_id,
        requested_actions: Vec::new(),
    };

    match reporter.report(&report).await {
        Ok(Some(new_id)) => {
            let mut a = evaluation.into_active_model();
            a.repo_check_id = Set(Some(new_id));
            if let Err(e) = a.update(&state.worker_db).await {
                warn!(error = %e, %evaluation_id, "Failed to persist evaluation check_run id");
            }
        }
        Ok(None) => {}
        Err(e) => warn!(
            error = format!("{e:#}"),
            "CI evaluation status report failed"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use sea_orm::{DatabaseBackend, MockDatabase};
    use test_support::prelude::test_state;

    fn make_eval(project: Option<ProjectId>) -> MEvaluation {
        entity::evaluation::Model {
            id: EvaluationId::now_v7(),
            project,
            repository: "https://example.com/repo".into(),
            commit: CommitId::now_v7(),
            wildcard: "*".into(),
            status: entity::evaluation::EvaluationStatus::Queued,
            previous: None,
            next: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
            flake_source: None,
            repo_check_id: None,
            waiting_reason: None,
            trigger: None,
            concurrent: false,
        }
    }

    fn empty_state() -> Arc<ServerState> {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        test_state(db)
    }

    #[tokio::test]
    async fn pending_ci_skips_when_eval_has_no_project() {
        let state = empty_state();
        let eval = make_eval(None);

        spawn_pending_ci_for_eval(Arc::clone(&state), &eval);

        assert_eq!(state.shutdown.pending(), 0, "no project ⇒ no spawned task");
    }

    #[tokio::test]
    async fn pending_ci_spawns_task_when_eval_has_project() {
        let state = empty_state();
        let eval = make_eval(Some(ProjectId::now_v7()));

        spawn_pending_ci_for_eval(Arc::clone(&state), &eval);

        assert!(
            state.shutdown.pending() >= 1,
            "project present ⇒ task tracked on shutdown",
        );

        // Drain the spawned task so it doesn't outlive the test. With an empty
        // mock DB the inner report bails on the first missing query and exits
        // cleanly, so a short timeout is enough.
        let _ = state
            .shutdown
            .cancel_and_drain(std::time::Duration::from_secs(1))
            .await;
    }
}
