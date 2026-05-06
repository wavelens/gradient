/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Orchestrates trigger fire → evaluation creation. Encapsulates commit-level
//! deduplication and concurrency policy. Callers: scheduler dispatch loop,
//! forge webhooks, manual API endpoints.

use super::abort::{abort_evaluation, AbortKind};
use super::trigger::{trigger_evaluation, TriggerError};
use crate::types::triggers::{ConcurrencyPolicy, TriggerType};
use crate::types::*;

use entity::evaluation::EvaluationStatus;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

#[derive(Debug)]
pub enum ApplyOutcome {
    Created {
        evaluation: MEvaluation,
        /// `Some(eval_id)` if a concurrency policy aborted an in-flight eval.
        /// The caller is responsible for calling `Scheduler::cancel_evaluation_jobs`
        /// for that eval to purge its in-memory `JobTracker` entries.
        aborted_evaluation: Option<EvaluationId>,
        /// Build IDs marked `Aborted` by a `HardAbort` policy. Empty for
        /// `SoftAbort` (builds keep running) and the no-abort path.
        aborted_builds: Vec<BuildId>,
    },
    SkippedSameCommit,
    SkippedConcurrency,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error(transparent)]
    Db(#[from] sea_orm::DbErr),
    #[error(transparent)]
    Trigger(#[from] TriggerError),
}

pub struct ApplyInput {
    pub trigger_id: ProjectTriggerId,
    pub trigger_type: TriggerType,
    pub commit_hash: Vec<u8>,
    pub commit_message: Option<String>,
    pub author_name: Option<String>,
    /// Set true for manual UI re-runs and `/triggers/{id}/test` calls.
    /// Bypasses the same-commit dedup check.
    pub manual: bool,
}

pub async fn apply_trigger<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
    input: ApplyInput,
) -> Result<ApplyOutcome, ApplyError> {
    let dedup_applies = !input.manual && input.trigger_type != TriggerType::Time;

    // Find any in-flight evaluation up-front; we use it for dedup against the
    // currently-running commit AND for the concurrency policy below.
    let active_codes: Vec<i32> = EvaluationStatus::ACTIVE
        .iter()
        .map(|s| s.num_value())
        .collect();
    let in_flight = EEvaluation::find()
        .filter(CEvaluation::Project.eq(project.id))
        .filter(CEvaluation::Status.is_in(active_codes))
        .one(db)
        .await?;

    // ── Same-commit dedup ─────────────────────────────────────────────────
    // Skip when:
    //   - an in-flight evaluation is already running on this commit
    //     (covers polling-while-build-is-running, even if last_evaluation
    //     is dangling or points elsewhere), OR
    //   - last_evaluation's commit matches (covers terminal-then-poll-again).
    // Time triggers and manual fires bypass this check.
    if dedup_applies {
        if let Some(running) = &in_flight {
            if let Some(running_commit) = ECommit::find_by_id(running.commit).one(db).await? {
                if running_commit.hash == input.commit_hash {
                    return Ok(ApplyOutcome::SkippedSameCommit);
                }
            }
        }

        if let Some(prev) = project.last_evaluation {
            if let Some(prev_eval) = EEvaluation::find_by_id(prev).one(db).await? {
                if let Some(prev_commit) = ECommit::find_by_id(prev_eval.commit).one(db).await? {
                    if prev_commit.hash == input.commit_hash {
                        return Ok(ApplyOutcome::SkippedSameCommit);
                    }
                }
            }
        }
    }

    // ── Concurrency policy ────────────────────────────────────────────────
    let concurrency = ConcurrencyPolicy::from_i16(project.concurrency)
        .unwrap_or(ConcurrencyPolicy::SoftAbort);

    let mut aborted_evaluation: Option<EvaluationId> = None;
    let mut aborted_builds: Vec<BuildId> = Vec::new();
    let concurrent_flag = matches!(concurrency, ConcurrencyPolicy::All);

    if !concurrent_flag {
        if let Some(running) = in_flight {
            match concurrency {
                ConcurrencyPolicy::Skip => return Ok(ApplyOutcome::SkippedConcurrency),
                ConcurrencyPolicy::HardAbort => {
                    aborted_builds =
                        abort_evaluation(db, running.id, AbortKind::Hard).await?;
                    aborted_evaluation = Some(running.id);
                }
                ConcurrencyPolicy::SoftAbort => {
                    abort_evaluation(db, running.id, AbortKind::Soft).await?;
                    aborted_evaluation = Some(running.id);
                }
                ConcurrencyPolicy::All => unreachable!("filtered above"),
            }
        }
    }

    let eval = match trigger_evaluation(
        db,
        project,
        input.commit_hash,
        input.commit_message,
        input.author_name,
        Some(input.trigger_id),
        concurrent_flag,
    )
    .await
    {
        Ok(e) => e,
        Err(TriggerError::AlreadyInProgress) => return Ok(ApplyOutcome::SkippedConcurrency),
        Err(TriggerError::Db(ref e))
            if e.to_string().contains("uq_evaluation_one_active_per_project") =>
        {
            return Ok(ApplyOutcome::SkippedConcurrency);
        }
        Err(e) => return Err(e.into()),
    };
    Ok(ApplyOutcome::Created { evaluation: eval, aborted_evaluation, aborted_builds })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDateTime;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult};
    use uuid::Uuid;

    fn make_project_with_last_eval(last: Option<EvaluationId>) -> MProject {
        make_project_with_concurrency(last, 3) // Skip
    }

    fn make_project_with_concurrency(last: Option<EvaluationId>, concurrency: i16) -> MProject {
        MProject {
            id: ProjectId::new(Uuid::parse_str("00000000-0000-0000-0000-000000000003").unwrap()),
            organization: OrganizationId::nil(),
            name: "test-project".into(),
            active: true,
            display_name: "Test".into(),
            description: "".into(),
            repository: "https://example/r".into(),
            evaluation_wildcard: "*".into(),
            last_evaluation: last,
            last_check_at: NaiveDateTime::default(),
            force_evaluation: false,
            created_by: UserId::nil(),
            created_at: NaiveDateTime::default(),
            managed: false,
            keep_evaluations: 10,
            concurrency,
        }
    }

    fn make_eval(
        id: EvaluationId,
        project: ProjectId,
        commit: CommitId,
        status: EvaluationStatus,
    ) -> entity::evaluation::Model {
        entity::evaluation::Model {
            id,
            project: Some(project),
            repository: "".into(),
            commit,
            wildcard: "*".into(),
            status,
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

    fn make_commit(id: CommitId, hash: Vec<u8>) -> entity::commit::Model {
        entity::commit::Model {
            id,
            message: "".into(),
            hash,
            author: None,
            author_name: "".into(),
        }
    }

    fn input(
        trig: ProjectTriggerId,
        ttype: TriggerType,
        hash: Vec<u8>,
        manual: bool,
    ) -> ApplyInput {
        ApplyInput {
            trigger_id: trig,
            trigger_type: ttype,
            commit_hash: hash,
            commit_message: None,
            author_name: None,
            manual,
        }
    }

    #[tokio::test]
    async fn skips_when_same_commit_as_last_eval() {
        let prev_eval_id = EvaluationId::now_v7();
        let prev_commit_id = CommitId::now_v7();
        let project = make_project_with_last_eval(Some(prev_eval_id));
        let same_hash = vec![1u8; 20];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Same-commit dedup: fetch prev eval
            .append_query_results([vec![make_eval(
                prev_eval_id,
                project.id,
                prev_commit_id,
                EvaluationStatus::Completed,
            )]])
            // Same-commit dedup: fetch prev commit
            .append_query_results([vec![make_commit(prev_commit_id, same_hash.clone())]])
            .into_connection();

        let trig = ProjectTriggerId::now_v7();
        let res = apply_trigger(
            &db,
            &project,
            input(trig, TriggerType::Polling, same_hash, false),
        )
        .await
        .unwrap();
        assert!(matches!(res, ApplyOutcome::SkippedSameCommit));
    }

    #[tokio::test]
    async fn time_trigger_bypasses_same_commit_check() {
        let prev_eval_id = EvaluationId::now_v7();
        let project = make_project_with_last_eval(Some(prev_eval_id));
        let same_hash = vec![1u8; 20];
        let new_eval_id = EvaluationId::now_v7();
        let new_commit_id = CommitId::now_v7();
        let trig = ProjectTriggerId::now_v7();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // No same-commit dedup queries (time bypasses)
            // Concurrency check: no in-flight
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            // trigger_evaluation internal in-progress check (none)
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            // trigger_evaluation: resolve previous (returns the prev eval row)
            .append_query_results([vec![make_eval(prev_eval_id, project.id, CommitId::nil(), EvaluationStatus::Completed)]])
            // commit insert
            .append_query_results([vec![make_commit(new_commit_id, same_hash.clone())]])
            // evaluation insert
            .append_query_results([vec![{
                let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
                m.trigger = Some(trig);
                m
            }]])
            // project update read-back
            .append_query_results([vec![project.clone()]])
            // project update exec
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();

        let res = apply_trigger(
            &db,
            &project,
            input(trig, TriggerType::Time, same_hash, false),
        )
        .await
        .unwrap();
        assert!(matches!(res, ApplyOutcome::Created { .. }));
    }

    #[tokio::test]
    async fn skip_concurrency_with_running_eval() {
        let project = make_project_with_last_eval(None);
        let running_eval_id = EvaluationId::now_v7();
        let running_eval =
            make_eval(running_eval_id, project.id, CommitId::nil(), EvaluationStatus::Building);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // in_flight lookup returns the running eval
            .append_query_results([vec![running_eval.clone()]])
            // dedup against running's commit: row missing → fall through
            .append_query_results([Vec::<entity::commit::Model>::new()])
            // No last_evaluation, so no further dedup queries.
            // Concurrency policy reuses the in-flight eval — Skip => SkippedConcurrency.
            .into_connection();

        let trig = ProjectTriggerId::now_v7();
        let res = apply_trigger(
            &db,
            &project,
            input(trig, TriggerType::Polling, vec![9u8; 20], false),
        )
        .await
        .unwrap();
        assert!(matches!(res, ApplyOutcome::SkippedConcurrency));
    }

    #[tokio::test]
    async fn polling_with_in_flight_same_commit_skips_without_aborting() {
        // Regression: a polling trigger that observes the same commit currently
        // being built must NOT abort the running evaluation. Even if
        // last_evaluation is dangling or missing, dedup against the in-flight
        // eval's commit catches it before the concurrency policy fires.
        let project = make_project_with_concurrency(None, 1); // SoftAbort
        let running_eval_id = EvaluationId::now_v7();
        let running_commit_id = CommitId::now_v7();
        let same_hash = vec![3u8; 20];
        let running_eval = make_eval(
            running_eval_id,
            project.id,
            running_commit_id,
            EvaluationStatus::Building,
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // in_flight lookup returns the running eval
            .append_query_results([vec![running_eval.clone()]])
            // dedup fetches the running eval's commit — same hash as the poll
            .append_query_results([vec![make_commit(running_commit_id, same_hash.clone())]])
            // dedup short-circuits with SkippedSameCommit; no abort, no insert
            .into_connection();

        let trig = ProjectTriggerId::now_v7();
        let res = apply_trigger(
            &db,
            &project,
            input(trig, TriggerType::Polling, same_hash, false),
        )
        .await
        .unwrap();
        assert!(
            matches!(res, ApplyOutcome::SkippedSameCommit),
            "expected SkippedSameCommit, got {res:?}"
        );
    }

    #[tokio::test]
    async fn all_concurrency_creates_evaluation_alongside_running() {
        let project = make_project_with_concurrency(None, 2); // All
        let new_eval_id = EvaluationId::now_v7();
        let new_commit_id = CommitId::now_v7();
        let trig = ProjectTriggerId::now_v7();
        let new_hash = vec![9u8; 20];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // in_flight lookup runs unconditionally — return empty for this test
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            // all policy skips the in-flight concurrency action
            // trigger_evaluation: concurrent=true skips the in-progress guard — no guard query
            // trigger_evaluation: resolve previous (no last_evaluation)
            // commit insert
            .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
            // evaluation insert — the new eval carries concurrent=true
            .append_query_results([vec![{
                let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
                m.trigger = Some(trig);
                m.concurrent = true;
                m
            }]])
            // project update read-back
            .append_query_results([vec![project.clone()]])
            // project update exec
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();

        let res = apply_trigger(
            &db,
            &project,
            input(trig, TriggerType::Polling, new_hash, false),
        )
        .await
        .unwrap();

        let ApplyOutcome::Created { evaluation, aborted_evaluation, aborted_builds } = res else {
            panic!("expected Created, got {res:?}");
        };
        assert_eq!(evaluation.id, new_eval_id);
        assert!(evaluation.concurrent, "new eval must carry concurrent=true");
        assert_eq!(aborted_evaluation, None);
        assert!(aborted_builds.is_empty());
    }

    #[tokio::test]
    async fn unique_constraint_violation_returns_skipped_concurrency() {
        let project = make_project_with_last_eval(None);
        let new_commit_id = CommitId::now_v7();
        let trig = ProjectTriggerId::now_v7();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // Concurrency check: no in-flight (races past the guard)
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            // trigger_evaluation: no in-progress guard
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            // commit insert
            .append_query_results([vec![make_commit(new_commit_id, vec![1u8; 20])]])
            // evaluation insert fails with unique constraint violation
            .append_query_errors([sea_orm::DbErr::Custom(
                "uq_evaluation_one_active_per_project".into(),
            )])
            .into_connection();

        let res = apply_trigger(
            &db,
            &project,
            input(trig, TriggerType::Polling, vec![1u8; 20], false),
        )
        .await
        .unwrap();
        assert!(
            matches!(res, ApplyOutcome::SkippedConcurrency),
            "expected SkippedConcurrency, got {res:?}"
        );
    }

    #[tokio::test]
    async fn manual_bypasses_same_commit_check() {
        let prev_eval_id = EvaluationId::now_v7();
        let project = make_project_with_last_eval(Some(prev_eval_id));
        let same_hash = vec![1u8; 20];
        let new_eval_id = EvaluationId::now_v7();
        let new_commit_id = CommitId::now_v7();
        let trig = ProjectTriggerId::now_v7();

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // manual=true skips same-commit dedup entirely
            // Concurrency check: no in-flight
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            // trigger_evaluation internal in-progress check
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            // trigger_evaluation: resolve previous (prev row exists)
            .append_query_results([vec![make_eval(prev_eval_id, project.id, CommitId::nil(), EvaluationStatus::Completed)]])
            // commit insert
            .append_query_results([vec![make_commit(new_commit_id, same_hash.clone())]])
            // evaluation insert
            .append_query_results([vec![make_eval(
                new_eval_id,
                project.id,
                new_commit_id,
                EvaluationStatus::Queued,
            )]])
            // project update read-back
            .append_query_results([vec![project.clone()]])
            // project update exec
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();

        let res = apply_trigger(
            &db,
            &project,
            input(trig, TriggerType::Polling, same_hash, true),
        )
        .await
        .unwrap();
        assert!(matches!(res, ApplyOutcome::Created { .. }));
    }

    #[tokio::test]
    async fn hard_abort_populates_aborted_fields() {
        let project = make_project_with_concurrency(None, 0); // HardAbort
        let running_eval_id = EvaluationId::now_v7();
        let running_eval =
            make_eval(running_eval_id, project.id, CommitId::nil(), EvaluationStatus::Building);
        let active_build_id = BuildId::now_v7();
        let active_build = entity::build::Model {
            id: active_build_id,
            evaluation: running_eval_id,
            derivation: DerivationId::nil(),
            status: entity::build::BuildStatus::Building,
            log_id: None,
            build_time_ms: None,
            worker: None,
            via: None,
            external_cached: false,
            created_at: chrono::NaiveDateTime::default(),
            updated_at: chrono::NaiveDateTime::default(),
        };
        let new_eval_id = EvaluationId::now_v7();
        let new_commit_id = CommitId::now_v7();
        let trig = ProjectTriggerId::now_v7();
        let new_hash = vec![7u8; 20];

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // in_flight lookup: returns the running eval
            .append_query_results([vec![running_eval.clone()]])
            // dedup fetches the running eval's commit — row missing → fall through
            .append_query_results([Vec::<entity::commit::Model>::new()])
            // abort_evaluation: eval fetch
            .append_query_results([vec![running_eval.clone()]])
            // abort_evaluation: eval update read-back
            .append_query_results([vec![running_eval.clone()]])
            // abort_evaluation: eval exec
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            // abort_evaluation: builds list
            .append_query_results([vec![active_build.clone()]])
            // abort_evaluation: active build refetch
            .append_query_results([vec![active_build.clone()]])
            // abort_evaluation: active build exec
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            // trigger_evaluation: in-progress guard
            .append_query_results([Vec::<entity::evaluation::Model>::new()])
            // trigger_evaluation: commit insert
            .append_query_results([vec![make_commit(new_commit_id, new_hash.clone())]])
            // trigger_evaluation: eval insert
            .append_query_results([vec![{
                let mut m = make_eval(new_eval_id, project.id, new_commit_id, EvaluationStatus::Queued);
                m.trigger = Some(trig);
                m
            }]])
            // trigger_evaluation: project update read-back
            .append_query_results([vec![project.clone()]])
            // trigger_evaluation: project exec
            .append_exec_results([MockExecResult { last_insert_id: 0, rows_affected: 1 }])
            .into_connection();

        let res = apply_trigger(
            &db,
            &project,
            input(trig, TriggerType::Polling, new_hash, false),
        )
        .await
        .unwrap();

        let ApplyOutcome::Created { evaluation, aborted_evaluation, aborted_builds } = res else {
            panic!("expected Created, got {res:?}");
        };
        assert_eq!(evaluation.id, new_eval_id);
        assert_eq!(aborted_evaluation, Some(running_eval_id));
        assert_eq!(aborted_builds, vec![active_build_id]);
    }
}
