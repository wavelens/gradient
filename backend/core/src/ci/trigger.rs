/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared logic for creating a queued evaluation from any trigger source
//! (API endpoint, incoming forge webhook, …).

use crate::types::consts::NULL_TIME;
use crate::types::*;
use chrono::Utc;
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder,
};
use thiserror::Error;
use uuid::Uuid;

#[derive(Debug, Error)]
pub enum TriggerError {
    #[error("evaluation already in progress for this project")]
    AlreadyInProgress,
    #[error("no previous evaluation found to restart from")]
    NoPreviousEvaluation,
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

/// Creates a new `Queued` evaluation for `project` at `commit_hash`.
///
/// - Refuses with [`TriggerError::AlreadyInProgress`] when the project already
///   has a running evaluation (Queued / Fetching / EvaluatingFlake /
///   EvaluatingDerivation / Building / Waiting).
/// - Inserts a `Commit` row, then an `Evaluation` row with status `Queued`.
/// - Sets `project.force_evaluation = true` and resets `last_check_at` so the
///   scheduler picks it up immediately on its next tick.
pub async fn trigger_evaluation(
    db: &DatabaseConnection,
    project: &MProject,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
) -> Result<MEvaluation, TriggerError> {
    // Guard: reject if an evaluation is already in progress.
    let in_progress = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(
            Condition::any()
                .add(CEvaluation::Status.eq(EvaluationStatus::Queued))
                .add(CEvaluation::Status.eq(EvaluationStatus::Fetching))
                .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingFlake))
                .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingDerivation))
                .add(CEvaluation::Status.eq(EvaluationStatus::Building))
                .add(CEvaluation::Status.eq(EvaluationStatus::Waiting)),
        )
        .one(db)
        .await?;

    if in_progress.is_some() {
        return Err(TriggerError::AlreadyInProgress);
    }

    let now = Utc::now().naive_utc();

    let acommit = ACommit {
        id: Set(Uuid::new_v4()),
        message: Set(commit_message.unwrap_or_default()),
        hash: Set(commit_hash),
        author: Set(None),
        author_name: Set(author_name.unwrap_or_default()),
    };
    let commit = acommit.insert(db).await?;

    let aevaluation = AEvaluation {
        id: Set(Uuid::new_v4()),
        project: Set(Some(project.id)),
        repository: Set(project.repository.clone()),
        commit: Set(commit.id),
        wildcard: Set(project.evaluation_wildcard.clone()),
        status: Set(EvaluationStatus::Queued),
        previous: Set(project.last_evaluation),
        next: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        flake_source: Set(None),
    };
    let evaluation = aevaluation.insert(db).await?;

    let mut aproject: AProject = project.clone().into();
    aproject.last_check_at = Set(*NULL_TIME);
    aproject.last_evaluation = Set(Some(evaluation.id));
    aproject.force_evaluation = Set(true);
    aproject.update(db).await?;

    Ok(evaluation)
}

/// Status mapping applied to each previous build when restarting.
///
/// Outputs already present in the cache are marked `Substituted` so the worker
/// skips them; everything else is re-queued for a fresh build.
pub(crate) fn restart_build_status(prev: BuildStatus) -> BuildStatus {
    match prev {
        BuildStatus::Completed | BuildStatus::Substituted => BuildStatus::Substituted,
        _ => BuildStatus::Queued,
    }
}

/// Creates a new `Building` evaluation that skips the fetch+eval phase and
/// re-runs only the failed builds from the most recent evaluation.
///
/// Status mapping from the previous build:
/// - `Completed` | `Substituted` → `Substituted`  (already in the cache; no rebuild needed)
/// - everything else             → `Queued`        (rebuild)
///
/// Entry points are copied from the previous evaluation and linked to the new builds.
/// The scheduler's build-dispatch loop will pick up the `Queued` builds on its next tick.
pub async fn trigger_restart_builds(
    db: &DatabaseConnection,
    project: &MProject,
) -> Result<MEvaluation, TriggerError> {
    // Guard: reject if an evaluation is already in progress.
    let in_progress = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(
            Condition::any()
                .add(CEvaluation::Status.eq(EvaluationStatus::Queued))
                .add(CEvaluation::Status.eq(EvaluationStatus::Fetching))
                .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingFlake))
                .add(CEvaluation::Status.eq(EvaluationStatus::EvaluatingDerivation))
                .add(CEvaluation::Status.eq(EvaluationStatus::Building))
                .add(CEvaluation::Status.eq(EvaluationStatus::Waiting)),
        )
        .one(db)
        .await?;

    if in_progress.is_some() {
        return Err(TriggerError::AlreadyInProgress);
    }

    // Find the most recent evaluation for the project.
    let prev_eval = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .order_by_desc(CEvaluation::CreatedAt)
        .one(db)
        .await?
        .ok_or(TriggerError::NoPreviousEvaluation)?;

    let now = Utc::now().naive_utc();

    // Create a new evaluation that starts directly in `Building` state.
    let new_eval_id = Uuid::new_v4();
    let aevaluation = AEvaluation {
        id: Set(new_eval_id),
        project: Set(Some(project.id)),
        repository: Set(prev_eval.repository.clone()),
        commit: Set(prev_eval.commit),
        wildcard: Set(prev_eval.wildcard.clone()),
        status: Set(EvaluationStatus::Building),
        previous: Set(Some(prev_eval.id)),
        next: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        flake_source: Set(prev_eval.flake_source.clone()),
    };
    let new_eval = aevaluation.insert(db).await?;

    // Load all builds from the previous evaluation.
    let prev_builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(prev_eval.id))
        .all(db)
        .await?;

    // Create new builds for the new evaluation and track old→new build ID mapping.
    let mut build_id_map: std::collections::HashMap<Uuid, Uuid> =
        std::collections::HashMap::with_capacity(prev_builds.len());

    for prev_build in &prev_builds {
        let new_status = restart_build_status(prev_build.status.clone());
        let new_build_id = Uuid::new_v4();
        let abuild = ABuild {
            id: Set(new_build_id),
            evaluation: Set(new_eval_id),
            derivation: Set(prev_build.derivation),
            status: Set(new_status),
            server: Set(None),
            log_id: Set(None),
            build_time_ms: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
        abuild.insert(db).await?;
        build_id_map.insert(prev_build.id, new_build_id);
    }

    // Copy entry points from the previous evaluation, remapping build IDs.
    let prev_entry_points = EEntryPoint::find()
        .filter(CEntryPoint::Evaluation.eq(prev_eval.id))
        .all(db)
        .await?;

    for prev_ep in prev_entry_points {
        if let Some(&new_build_id) = build_id_map.get(&prev_ep.build) {
            let aep = AEntryPoint {
                id: Set(Uuid::new_v4()),
                project: Set(prev_ep.project),
                evaluation: Set(new_eval_id),
                build: Set(new_build_id),
                eval: Set(prev_ep.eval),
                created_at: Set(now),
            };
            aep.insert(db).await?;
        }
    }

    // Update the project to point at the new evaluation.
    let mut aproject: AProject = project.clone().into();
    aproject.last_evaluation = Set(Some(new_eval_id));
    aproject.update(db).await?;

    Ok(new_eval)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use entity::evaluation;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};

    fn make_project() -> MProject {
        MProject {
            id: Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap(),
            organization: Uuid::nil(),
            name: "test-project".into(),
            active: true,
            display_name: "Test Project".into(),
            description: "".into(),
            repository: "https://github.com/test/repo".into(),
            evaluation_wildcard: "*".into(),
            last_evaluation: None,
            last_check_at: NaiveDateTime::default(),
            force_evaluation: false,
            created_by: Uuid::nil(),
            created_at: NaiveDateTime::default(),
            managed: false,
            keep_evaluations: 10,
            ci_reporter_type: None,
            ci_reporter_url: None,
            ci_reporter_token: None,
        }
    }

    fn make_eval(id: Uuid, status: EvaluationStatus) -> evaluation::Model {
        evaluation::Model {
            id,
            project: Some(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()),
            repository: "https://github.com/test/repo".into(),
            commit: Uuid::nil(),
            wildcard: "*".into(),
            status,
            previous: None,
            next: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
            flake_source: None,
        }
    }

    #[tokio::test]
    async fn trigger_creates_queued_eval() {
        let project = make_project();
        let eval_id = Uuid::new_v4();
        let commit_id = Uuid::new_v4();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1st SELECT: no in-progress evaluations
            .append_query_results([Vec::<evaluation::Model>::new()])
            // INSERT commit → returns commit row
            .append_query_results([vec![entity::commit::Model {
                id: commit_id,
                message: "".into(),
                hash: vec![0u8; 20],
                author: None,
                author_name: "".into(),
            }]])
            // INSERT evaluation → returns evaluation row
            .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Queued)]])
            // SELECT project for update
            .append_query_results([vec![project.clone()]])
            // UPDATE project → exec result
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result = trigger_evaluation(&db, &project, vec![0u8; 20], None, None).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        assert_eq!(result.unwrap().status, EvaluationStatus::Queued);
    }

    #[tokio::test]
    async fn trigger_already_in_progress() {
        let project = make_project();
        let existing_eval = make_eval(Uuid::new_v4(), EvaluationStatus::Queued);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1st SELECT: returns in-progress evaluation
            .append_query_results([vec![existing_eval]])
            .into_connection();

        let result = trigger_evaluation(&db, &project, vec![0u8; 20], None, None).await;
        assert!(matches!(result, Err(TriggerError::AlreadyInProgress)));
    }

    #[tokio::test]
    async fn trigger_each_active_status_blocks() {
        let active_statuses = [
            EvaluationStatus::Fetching,
            EvaluationStatus::EvaluatingFlake,
            EvaluationStatus::EvaluatingDerivation,
            EvaluationStatus::Building,
            EvaluationStatus::Waiting,
        ];

        for status in active_statuses {
            let project = make_project();
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![make_eval(Uuid::new_v4(), status.clone())]])
                .into_connection();
            let result = trigger_evaluation(&db, &project, vec![0u8; 20], None, None).await;
            assert!(
                matches!(result, Err(TriggerError::AlreadyInProgress)),
                "{status:?} should block trigger"
            );
        }
    }

    // ── restart_build_status ─────────────────────────────────────────────────

    #[test]
    fn restart_status_cached_stays_substituted() {
        assert_eq!(
            restart_build_status(BuildStatus::Completed),
            BuildStatus::Substituted,
        );
        assert_eq!(
            restart_build_status(BuildStatus::Substituted),
            BuildStatus::Substituted,
        );
    }

    #[test]
    fn restart_status_others_become_queued() {
        for s in [
            BuildStatus::Queued,
            BuildStatus::Building,
            BuildStatus::Failed,
            BuildStatus::Aborted,
            BuildStatus::Created,
            BuildStatus::DependencyFailed,
        ] {
            assert_eq!(
                restart_build_status(s.clone()),
                BuildStatus::Queued,
                "{s:?} should be re-queued"
            );
        }
    }

    #[tokio::test]
    async fn trigger_terminal_does_not_block() {
        let project = make_project();
        let eval_id = Uuid::new_v4();
        let commit_id = Uuid::new_v4();

        // Terminal status in DB should not block a new trigger
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1st SELECT: returns completed evaluation (terminal → not in-progress)
            .append_query_results([Vec::<evaluation::Model>::new()])
            .append_query_results([vec![entity::commit::Model {
                id: commit_id,
                message: "".into(),
                hash: vec![0u8; 20],
                author: None,
                author_name: "".into(),
            }]])
            .append_query_results([vec![make_eval(eval_id, EvaluationStatus::Queued)]])
            .append_query_results([vec![project.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result = trigger_evaluation(&db, &project, vec![0u8; 20], None, None).await;
        assert!(result.is_ok(), "terminal eval should not block new trigger");
    }
}
