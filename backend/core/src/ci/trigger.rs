/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared logic for creating a queued evaluation from any trigger source
//! (API endpoint, incoming forge webhook, …).

use crate::types::consts::NULL_TIME;
use crate::types::*;

use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use sea_orm::ActiveValue::Set;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder,
};
use thiserror::Error;

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
/// - When `concurrent` is false, refuses with [`TriggerError::AlreadyInProgress`]
///   if the project already has a running evaluation (Queued / Fetching /
///   EvaluatingFlake / EvaluatingDerivation / Building / Waiting).
/// - When `concurrent` is true (used by the `all` concurrency policy), skips
///   the in-progress guard and sets `evaluation.concurrent = true` on the new
///   row so the partial unique index lets it through.
/// - Inserts a `Commit` row, then an `Evaluation` row with status `Queued`.
/// - Sets `project.force_evaluation = true` and resets `last_check_at` so the
///   scheduler picks it up immediately on its next tick.
#[allow(clippy::too_many_arguments)]
pub async fn trigger_evaluation<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    commit_hash: Vec<u8>,
    commit_message: Option<String>,
    author_name: Option<String>,
    trigger: Option<crate::types::ids::ProjectTriggerId>,
    concurrent: bool,
    repository_override: Option<String>,
    wildcard_override: Option<String>,
    source_comment: Option<serde_json::Value>,
) -> Result<MEvaluation, TriggerError> {
    if !concurrent {
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
    }

    // Resolve `project.last_evaluation` against the DB so a dangling pointer
    // (eval row gone but the project pointer still set) doesn't trip the
    // `fk-evaluation-previous` foreign key.
    let previous = match project.last_evaluation {
        Some(prev_id) => EEvaluation::find_by_id(prev_id)
            .one(db)
            .await?
            .map(|e| e.id),
        None => None,
    };

    let now = crate::types::now();

    let acommit = ACommit {
        id: Set(CommitId::now_v7()),
        message: Set(commit_message.unwrap_or_default()),
        hash: Set(commit_hash),
        author: Set(None),
        author_name: Set(author_name.unwrap_or_default()),
    };
    let commit = acommit.insert(db).await?;

    let aevaluation = AEvaluation {
        id: Set(EvaluationId::now_v7()),
        project: Set(Some(project.id)),
        repository: Set(repository_override.unwrap_or_else(|| project.repository.clone())),
        commit: Set(commit.id),
        wildcard: Set(wildcard_override.unwrap_or_else(|| project.wildcard.clone())),
        status: Set(EvaluationStatus::Queued),
        previous: Set(previous),
        next: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        flake_source: Set(None),
        check_run_ids: Set(None),
        waiting_reason: Set(None),
        trigger: Set(trigger),
        concurrent: Set(concurrent),
        source_comment: Set(source_comment),
    };
    let evaluation = aevaluation.insert(db).await?;

    snapshot_flake_input_overrides(db, project.id, evaluation.id).await?;

    let mut aproject: AProject = project.clone().into();
    aproject.last_check_at = Set(*NULL_TIME);
    aproject.last_evaluation = Set(Some(evaluation.id));
    aproject.force_evaluation = Set(true);
    aproject.update(db).await?;

    Ok(evaluation)
}

pub(super) async fn snapshot_flake_input_overrides<C: ConnectionTrait>(
    txn: &C,
    project_id: entity::ids::ProjectId,
    evaluation_id: entity::ids::EvaluationId,
) -> Result<(), sea_orm::DbErr> {
    let rows = EProjectFlakeInputOverride::find()
        .filter(CProjectFlakeInputOverride::Project.eq(project_id))
        .all(txn)
        .await?;

    for r in rows {
        let am = AEvaluationFlakeInputOverride {
            id: Set(EvaluationFlakeInputOverrideId::now_v7()),
            evaluation: Set(evaluation_id),
            input_name: Set(r.input_name),
            url: Set(r.url),
        };
        am.insert(txn).await?;
    }
    Ok(())
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
pub async fn trigger_restart_builds<C: ConnectionTrait>(
    db: &C,
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

    let now = crate::types::now();

    // Load the previous evaluation's builds *before* inserting the new
    // evaluation so we can decide its initial status. If every previous build
    // maps to `Substituted` (i.e. there is nothing to actually rebuild), we
    // insert the evaluation as `Completed`; otherwise it starts in `Building`
    // and the scheduler's `check_evaluation_done` will close it out as the
    // queued builds finish.
    //
    // Without this, an all-`Substituted` restart would leave the evaluation
    // stuck in `Building` forever - no build job ever runs, so nothing fires
    // the completion check.
    let prev_builds = EBuild::find()
        .filter(CBuild::Evaluation.eq(prev_eval.id))
        .all(db)
        .await?;

    let any_pending = prev_builds
        .iter()
        .any(|b| !matches!(restart_build_status(b.status), BuildStatus::Substituted));
    let initial_status = if any_pending {
        EvaluationStatus::Building
    } else {
        EvaluationStatus::Completed
    };

    // Create the new evaluation with the computed initial status.
    let new_eval_id = EvaluationId::now_v7();
    let aevaluation = AEvaluation {
        id: Set(new_eval_id),
        project: Set(Some(project.id)),
        repository: Set(prev_eval.repository.clone()),
        commit: Set(prev_eval.commit),
        wildcard: Set(prev_eval.wildcard.clone()),
        status: Set(initial_status),
        previous: Set(Some(prev_eval.id)),
        next: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
        flake_source: Set(prev_eval.flake_source.clone()),
        check_run_ids: Set(None),
        waiting_reason: Set(None),
        trigger: Set(None),
        concurrent: Set(false),
        source_comment: Set(None),
    };
    let new_eval = aevaluation.insert(db).await?;

    snapshot_flake_input_overrides(db, project.id, new_eval.id).await?;

    // Look up any in-flight leader (Created/Queued/Building) in another
    // evaluation for the derivations we're about to rebuild. Restarting must
    // honour the same cross-evaluation dedup as the regular eval-result path,
    // otherwise two projects in the same organisation race for the Nix store
    // lock when one restarts while the other is still building.
    let queued_drv_ids: Vec<DerivationId> = prev_builds
        .iter()
        .filter(|b| !matches!(restart_build_status(b.status), BuildStatus::Substituted))
        .map(|b| b.derivation)
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();
    let leader_for_drv =
        crate::db::find_active_leaders(db, project.organization, &queued_drv_ids).await?;

    // Create new builds for the new evaluation and track old→new build ID mapping.
    let mut build_id_map: std::collections::HashMap<BuildId, BuildId> =
        std::collections::HashMap::with_capacity(prev_builds.len());

    for prev_build in &prev_builds {
        let new_status = restart_build_status(prev_build.status);
        let new_build_id = BuildId::now_v7();
        let via = if matches!(new_status, BuildStatus::Substituted) {
            None
        } else {
            leader_for_drv.get(&prev_build.derivation).copied()
        };
        let log_id = if matches!(new_status, BuildStatus::Substituted) {
            Some(prev_build.log_id.unwrap_or(prev_build.id))
        } else {
            None
        };
        let abuild = ABuild {
            id: Set(new_build_id),
            evaluation: Set(new_eval_id),
            derivation: Set(prev_build.derivation),
            status: Set(new_status),
            log_id: Set(log_id),
            build_time_ms: Set(None),
            worker: Set(None),
            via: Set(via),
            external_cached: Set(false),
            attempt: Set(0),
            timeout_secs: Set(None),
            max_silent_secs: Set(None),
            prefer_local_build: Set(false),
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
                id: Set(EntryPointId::now_v7()),
                project: Set(prev_ep.project),
                evaluation: Set(new_eval_id),
                build: Set(new_build_id),
                eval: Set(prev_ep.eval),
                created_at: Set(now),
                repo_check_id: Set(None),
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
    use uuid::Uuid;

    fn make_project() -> MProject {
        MProject {
            id: ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()),
            organization: OrganizationId::nil(),
            name: "test-project".into(),
            active: true,
            display_name: "Test Project".into(),
            description: "".into(),
            repository: "https://github.com/test/repo".into(),
            wildcard: "*".into(),
            last_evaluation: None,
            last_check_at: NaiveDateTime::default(),
            force_evaluation: false,
            created_by: UserId::nil(),
            created_at: NaiveDateTime::default(),
            managed: false,
            keep_evaluations: 10,
            concurrency: 3,
            sign_cache: true,
        }
    }

    fn make_eval(id: EvaluationId, status: EvaluationStatus) -> evaluation::Model {
        evaluation::Model {
            id,
            project: Some(ProjectId::new(
                Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap(),
            )),
            repository: "https://github.com/test/repo".into(),
            commit: CommitId::nil(),
            wildcard: "*".into(),
            status,
            previous: None,
            next: None,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
            flake_source: None,
            check_run_ids: None,
            waiting_reason: None,
            trigger: None,
            concurrent: false,
            source_comment: None,
        }
    }

    #[tokio::test]
    async fn trigger_creates_queued_eval() {
        let project = make_project();
        let eval_id = EvaluationId::now_v7();
        let commit_id = CommitId::now_v7();

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
            // SELECT project flake input overrides for snapshot (none)
            .append_query_results([Vec::<entity::project_flake_input_override::Model>::new()])
            // SELECT project for update
            .append_query_results([vec![project.clone()]])
            // UPDATE project → exec result
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result =
            trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        assert_eq!(result.unwrap().status, EvaluationStatus::Queued);
    }

    #[tokio::test]
    async fn trigger_drops_dangling_last_evaluation_pointer() {
        // Project points at an evaluation row that no longer exists. The
        // resolved `previous` must fall back to None so the FK doesn't fire.
        let stale_eval_id = EvaluationId::now_v7();
        let mut project = make_project();
        project.last_evaluation = Some(stale_eval_id);

        let new_eval_id = EvaluationId::now_v7();
        let commit_id = CommitId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // in-progress check: none active
            .append_query_results([Vec::<evaluation::Model>::new()])
            // resolve previous: row missing
            .append_query_results([Vec::<evaluation::Model>::new()])
            // insert commit
            .append_query_results([vec![entity::commit::Model {
                id: commit_id,
                message: "".into(),
                hash: vec![0u8; 20],
                author: None,
                author_name: "".into(),
            }]])
            // insert evaluation (previous should be None despite stale pointer)
            .append_query_results([vec![make_eval(new_eval_id, EvaluationStatus::Queued)]])
            // snapshot flake input overrides (none)
            .append_query_results([Vec::<entity::project_flake_input_override::Model>::new()])
            // project update read-back + exec
            .append_query_results([vec![project.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result =
            trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
    }

    #[tokio::test]
    async fn trigger_already_in_progress() {
        let project = make_project();
        let existing_eval = make_eval(EvaluationId::now_v7(), EvaluationStatus::Queued);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1st SELECT: returns in-progress evaluation
            .append_query_results([vec![existing_eval]])
            .into_connection();

        let result =
            trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None).await;
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
                .append_query_results([vec![make_eval(EvaluationId::now_v7(), status)]])
                .into_connection();
            let result =
                trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None)
                    .await;
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
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTransient,
            BuildStatus::FailedTimeout,
            BuildStatus::Aborted,
            BuildStatus::Created,
            BuildStatus::DependencyFailed,
        ] {
            assert_eq!(
                restart_build_status(s),
                BuildStatus::Queued,
                "{s:?} should be re-queued"
            );
        }
    }

    #[tokio::test]
    async fn trigger_terminal_does_not_block() {
        let project = make_project();
        let eval_id = EvaluationId::now_v7();
        let commit_id = CommitId::now_v7();

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
            .append_query_results([Vec::<entity::project_flake_input_override::Model>::new()])
            .append_query_results([vec![project.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result =
            trigger_evaluation(&db, &project, vec![0u8; 20], None, None, None, false, None, None, None).await;
        assert!(result.is_ok(), "terminal eval should not block new trigger");
    }

    #[tokio::test]
    async fn trigger_records_trigger_id() {
        let project = make_project();
        let trig = ProjectTriggerId::now_v7();
        let eval_id = EvaluationId::now_v7();
        let commit_id = CommitId::now_v7();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<evaluation::Model>::new()])
            .append_query_results([vec![entity::commit::Model {
                id: commit_id,
                message: "".into(),
                hash: vec![0u8; 20],
                author: None,
                author_name: "".into(),
            }]])
            .append_query_results([vec![{
                let mut m = make_eval(eval_id, EvaluationStatus::Queued);
                m.trigger = Some(trig);
                m
            }]])
            .append_query_results([Vec::<entity::project_flake_input_override::Model>::new()])
            .append_query_results([vec![project.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result = trigger_evaluation(
            &db,
            &project,
            vec![0u8; 20],
            None,
            None,
            Some(trig),
            false,
            None,
            None,
            None,
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().trigger, Some(trig));
    }

    // ── trigger_restart_builds ───────────────────────────────────────────────

    fn make_build(id: BuildId, eval_id: EvaluationId, status: BuildStatus) -> entity::build::Model {
        make_build_drv(id, eval_id, DerivationId::now_v7(), status)
    }

    fn make_build_drv(
        id: BuildId,
        eval_id: EvaluationId,
        derivation: DerivationId,
        status: BuildStatus,
    ) -> entity::build::Model {
        entity::build::Model {
            id,
            evaluation: eval_id,
            derivation,
            status,
            log_id: None,
            build_time_ms: None,
            worker: None,
            via: None,
            external_cached: false,
            attempt: 0,
            timeout_secs: None,
            max_silent_secs: None,
            prefer_local_build: false,
            created_at: NaiveDateTime::default(),
            updated_at: NaiveDateTime::default(),
        }
    }

    /// Regression for the "evaluations stuck in Building forever" symptom:
    /// when every previous build is `Completed`/`Substituted`,
    /// `restart_build_status` maps them all to `Substituted` (terminal) and
    /// no build job is ever dispatched. The new evaluation must therefore
    /// start in `Completed`, not `Building`, otherwise nothing fires
    /// `check_evaluation_done` and the row is stuck.
    #[tokio::test]
    async fn restart_with_all_cached_inserts_completed_eval() {
        let project = make_project();
        let prev_eval_id = EvaluationId::now_v7();
        let prev_eval = make_eval(prev_eval_id, EvaluationStatus::Completed);
        let new_eval_id = EvaluationId::now_v7();

        let prev_builds = vec![
            make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::Completed),
            make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::Substituted),
            make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::Completed),
        ];

        let inserted_eval = {
            let mut e = make_eval(new_eval_id, EvaluationStatus::Completed);
            e.previous = Some(prev_eval_id);
            e
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // 1. in-progress check: none
            .append_query_results([Vec::<evaluation::Model>::new()])
            // 2. find prev_eval
            .append_query_results([vec![prev_eval]])
            // 3. load prev_builds (all terminal)
            .append_query_results([prev_builds])
            // 4. INSERT new evaluation → returns the row with status=Completed
            .append_query_results([vec![inserted_eval]])
            // snapshot flake input overrides (none)
            .append_query_results([Vec::<entity::project_flake_input_override::Model>::new()])
            // 5. INSERT 3 builds (each returns the inserted row; we don't read back)
            .append_query_results([vec![make_build(
                BuildId::now_v7(),
                new_eval_id,
                BuildStatus::Substituted,
            )]])
            .append_query_results([vec![make_build(
                BuildId::now_v7(),
                new_eval_id,
                BuildStatus::Substituted,
            )]])
            .append_query_results([vec![make_build(
                BuildId::now_v7(),
                new_eval_id,
                BuildStatus::Substituted,
            )]])
            // 6. SELECT entry points: none
            .append_query_results([Vec::<entity::entry_point::Model>::new()])
            // 7. SELECT project for update read-back
            .append_query_results([vec![project.clone()]])
            // 8. UPDATE project
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result = trigger_restart_builds(&db, &project).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        assert_eq!(
            result.unwrap().status,
            EvaluationStatus::Completed,
            "all-cached restart must start the new eval as Completed, not Building",
        );
    }

    /// When at least one previous build maps to `Queued`, the new eval must
    /// start in `Building` so the dispatcher picks it up and the eventual
    /// `check_evaluation_done` flips it to its terminal state.
    #[tokio::test]
    async fn restart_with_one_failed_inserts_building_eval() {
        let project = make_project();
        let prev_eval_id = EvaluationId::now_v7();
        let prev_eval = make_eval(prev_eval_id, EvaluationStatus::Failed);
        let new_eval_id = EvaluationId::now_v7();

        let prev_builds = vec![
            make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::Completed),
            make_build(BuildId::now_v7(), prev_eval_id, BuildStatus::FailedPermanent),
        ];

        let inserted_eval = {
            let mut e = make_eval(new_eval_id, EvaluationStatus::Building);
            e.previous = Some(prev_eval_id);
            e
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<evaluation::Model>::new()])
            .append_query_results([vec![prev_eval]])
            .append_query_results([prev_builds])
            .append_query_results([vec![inserted_eval]])
            // snapshot flake input overrides (none)
            .append_query_results([Vec::<entity::project_flake_input_override::Model>::new()])
            // find_active_leaders for the one Queued drv → no in-flight leader.
            //   same-org pass: empty
            //   cross-org pass: empty derivation lookup short-circuits
            .append_query_results([Vec::<entity::build::Model>::new()])
            .append_query_results([Vec::<entity::derivation::Model>::new()])
            .append_query_results([vec![make_build(
                BuildId::now_v7(),
                new_eval_id,
                BuildStatus::Substituted,
            )]])
            .append_query_results([vec![make_build(
                BuildId::now_v7(),
                new_eval_id,
                BuildStatus::Queued,
            )]])
            .append_query_results([Vec::<entity::entry_point::Model>::new()])
            .append_query_results([vec![project.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result = trigger_restart_builds(&db, &project).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
        assert_eq!(result.unwrap().status, EvaluationStatus::Building);
    }

    /// Restarting must honour the cross-evaluation `via` dedup: if another
    /// evaluation (typically a different project in the same organisation)
    /// is currently building one of the drvs being restarted, the new build
    /// row follows that leader instead of racing it. Regression for the
    /// "rerun failed builds" path bypassing `find_active_leaders`.
    #[tokio::test]
    async fn restart_sets_via_when_leader_active_elsewhere() {
        let project = make_project();
        let prev_eval_id = EvaluationId::now_v7();
        let prev_eval = make_eval(prev_eval_id, EvaluationStatus::Failed);
        let new_eval_id = EvaluationId::now_v7();

        let shared_drv = DerivationId::now_v7();
        let prev_build = make_build_drv(
            BuildId::now_v7(),
            prev_eval_id,
            shared_drv,
            BuildStatus::FailedPermanent,
        );

        // Leader currently Building under a different evaluation.
        let other_eval_id = EvaluationId::now_v7();
        let leader = make_build_drv(
            BuildId::now_v7(),
            other_eval_id,
            shared_drv,
            BuildStatus::Building,
        );
        let leader_id = leader.id;

        let inserted_eval = {
            let mut e = make_eval(new_eval_id, EvaluationStatus::Building);
            e.previous = Some(prev_eval_id);
            e
        };

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<evaluation::Model>::new()])
            .append_query_results([vec![prev_eval]])
            .append_query_results([vec![prev_build]])
            .append_query_results([vec![inserted_eval]])
            // snapshot flake input overrides (none)
            .append_query_results([Vec::<entity::project_flake_input_override::Model>::new()])
            // find_active_leaders for [shared_drv] → returns the in-flight leader.
            .append_query_results([vec![leader]])
            // INSERT new build (with via=leader_id).
            .append_query_results([vec![{
                let mut b = make_build_drv(
                    BuildId::now_v7(),
                    new_eval_id,
                    shared_drv,
                    BuildStatus::Queued,
                );
                b.via = Some(leader_id);
                b
            }]])
            .append_query_results([Vec::<entity::entry_point::Model>::new()])
            .append_query_results([vec![project.clone()]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();

        let result = trigger_restart_builds(&db, &project).await;
        assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());

        // Verify the INSERT carried via=leader_id by inspecting the executed
        // statements. MockDatabase records every statement; the build insert
        // is the only one whose SQL mentions the `via` column.
        let logs = db.into_transaction_log();
        let build_insert = logs
            .iter()
            .flat_map(|t| t.statements())
            .find(|s| {
                let sql = s.sql.to_lowercase();
                sql.contains("insert into") && sql.contains("\"build\"") && sql.contains("\"via\"")
            })
            .expect("expected an INSERT INTO build statement");
        let values: Vec<String> = build_insert
            .values
            .as_ref()
            .map(|v| v.0.iter().map(|val| format!("{:?}", val)).collect())
            .unwrap_or_default();
        let joined = values.join(", ");
        assert!(
            joined.contains(&leader_id.into_inner().to_string()),
            "expected build INSERT to carry via={} (leader id), got values: {}",
            leader_id,
            joined,
        );
    }
}
