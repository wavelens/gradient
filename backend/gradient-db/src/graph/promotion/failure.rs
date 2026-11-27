/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::{
    predicates::{aborted_in_evaluation, failure_spreads_predicate},
    walks::{ClosureDirection, bounded_dependency_closure_cte_body, eval_closure_cte_body},
};
use crate::sql::StatusWrite;
use crate::status::TransitionChange;

use super::transitions::returned_transitions;
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};

fn cascade_dependency_failed_sql() -> String {
    let cte = format!(
        "WITH RECURSIVE {}",
        bounded_dependency_closure_cte_body(
            "wanted_by",
            "SELECT $1::uuid",
            ClosureDirection::WantedBy,
            &failure_spreads_predicate("e"),
            None,
        )
    );
    format!(
        r#"
    {cte}
    UPDATE derivation_build AS db
    SET status = {dependency_failed}, updated_at = (now() AT TIME ZONE 'UTC')
    WHERE db.status IN ({cascade_target}) AND {not_in_flight}
      AND db.derivation IN (SELECT derivation FROM wanted_by WHERE derivation <> $1)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#,
        dependency_failed = crate::sql::status::to(BuildStatus::CASCADE_DEPENDENCY_FAILED),
        cascade_target = crate::sql::status::from(BuildStatus::CASCADE_DEPENDENCY_FAILED),
        not_in_flight = not_in_flight(),
    )
}

fn not_in_flight() -> String {
    crate::scheduling::assignment_record::no_open_assignment_predicate(
        &crate::scheduling::assignment_record::build_job_key_sql("db.id"),
    )
}

crate::sql_fn! {
    CASCADE_DEPENDENCY_FAILED = cascade_dependency_failed_sql,
        params = [DerivationId],
        tier = Walk,
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::CASCADE_DEPENDENCY_FAILED);
}

pub async fn cascade_dependency_failed<C>(
    db: &C,
    failed_derivation: DerivationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    // The unbounded upward walk is the widest frontier in the system.
    // It is getting the raised `work_mem`.
    let walk = crate::graph::walks::begin_walk(db, &CASCADE_DEPENDENCY_FAILED).await?;
    let rows = walk
        .query_all_raw(
            CASCADE_DEPENDENCY_FAILED.bind([Value::Uuid(Some(failed_derivation.into_inner()))]),
        )
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

/// The reactive cascade is firing only on a fresh failure transition.
/// A thaw or a concurrent re-fail can leave a parent pending above a failed dependency.
/// This sweep is catching that parent.
pub async fn repair_dependency_failed<C>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db, &DEPENDENCY_FAILED_REPAIR).await?;
    let rows = walk
        .query_all_raw(DEPENDENCY_FAILED_REPAIR.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

fn dependency_failed_repair_sql() -> String {
    let dependency_failed = crate::sql::status::to(BuildStatus::CASCADE_DEPENDENCY_FAILED);
    let cascade_target = crate::sql::status::from(BuildStatus::CASCADE_DEPENDENCY_FAILED);
    let terminal_failure = crate::sql::status::build_in(&BuildStatus::TERMINAL_FAILURE);
    let prelude = format!(
        "WITH RECURSIVE {closure},\n    {wanted_by}",
        closure = eval_closure_cte_body(),
        wanted_by = bounded_dependency_closure_cte_body(
            "wanted_by",
            &format!(
                "SELECT derivation FROM (SELECT derivation FROM derivation_build \
                 WHERE status IN ({terminal_failure}) \
                   AND derivation IN (SELECT derivation FROM closure) \
                 UNION {aborted}) seed",
                aborted = aborted_in_evaluation("$1"),
            ),
            ClosureDirection::WantedBy,
            &failure_spreads_predicate("e"),
            Some("closure"),
        ),
    );

    format!(
        r#"
    {prelude}
    UPDATE derivation_build AS db
    SET status = {dependency_failed}, updated_at = (now() AT TIME ZONE 'UTC')
    WHERE db.status IN ({cascade_target}) AND {not_in_flight}
      AND db.derivation IN (SELECT derivation FROM wanted_by)
      AND db.derivation IN (SELECT derivation FROM closure)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#,
        not_in_flight = not_in_flight(),
    )
}

crate::sql_fn! {
    DEPENDENCY_FAILED_REPAIR = dependency_failed_repair_sql,
        params = [EvaluationId],
        tier = Walk,
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::CASCADE_DEPENDENCY_FAILED);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::EvaluationId;

    #[tokio::test]
    async fn repair_dependency_failed_fails_pending_parents_above_failed_dependency_and_leaves_other_builds_untouched()
     {
        use gradient_entity::build_attempt::AttemptOutcome;

        let path = |c: &str| format!("/nix/store/{}-repair.drv", c.repeat(32));
        let [failed, pending, completed, in_flight, outside] = ["1", "2", "3", "4", "5"];
        let (evaluation, other_evaluation) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let db = gradient_fixtures::shape::shape()
            .evaluation(evaluation)
            .derivation(&path(failed))
            .walked()
            .job()
            .output(&failed.repeat(32))
            .build_status(BuildStatus::FailedPermanent)
            .derivation(&path(pending))
            .walked()
            .job()
            .output(&pending.repeat(32))
            .depends_on(&path(failed))
            .derivation(&path(completed))
            .walked()
            .job()
            .output(&completed.repeat(32))
            .depends_on(&path(failed))
            .build_status(BuildStatus::Completed)
            .cached_path(&completed.repeat(32), "completed")
            .derivation(&path(in_flight))
            .walked()
            .job()
            .output(&in_flight.repeat(32))
            .depends_on(&path(failed))
            .build_status(BuildStatus::Building)
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .evaluation(other_evaluation)
            .derivation(&path(outside))
            .walked()
            .job()
            .output(&outside.repeat(32))
            .depends_on(&path(failed))
            .into_database()
            .await;

        let changes = repair_dependency_failed(&db.connection(), evaluation)
            .await
            .unwrap();
        let mut statuses = Vec::new();
        for key in [failed, pending, completed, in_flight, outside] {
            statuses.push(db.shared_build(&path(key)).await.status);
        }
        db.finish().await;

        assert_eq!(
            statuses,
            [
                BuildStatus::FailedPermanent,
                BuildStatus::DependencyFailed,
                BuildStatus::Completed,
                BuildStatus::Building,
                BuildStatus::Created
            ]
        );
        assert_eq!(changes.len(), 1);
    }

    #[tokio::test]
    async fn repair_dependency_failed_fails_parents_above_evaluation_aborted_build_but_not_above_build_aborted_elsewhere()
     {
        let path = |c: &str| format!("/nix/store/{}-aborted.drv", c.repeat(32));
        let [
            aborted_here,
            parent_here,
            aborted_elsewhere,
            parent_elsewhere,
        ] = ["1", "2", "3", "4"];
        let evaluation = EvaluationId::now_v7();
        let db = gradient_fixtures::shape::shape()
            .evaluation(evaluation)
            .derivation(&path(aborted_here))
            .walked()
            .job()
            .build_status(BuildStatus::Aborted)
            .derivation(&path(parent_here))
            .walked()
            .job()
            .depends_on(&path(aborted_here))
            .derivation(&path(aborted_elsewhere))
            .walked()
            .job()
            .build_status(BuildStatus::Aborted)
            .derivation(&path(parent_elsewhere))
            .walked()
            .job()
            .depends_on(&path(aborted_elsewhere))
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE build_job SET aborted = true WHERE derivation = '{}'",
            db.id_of(&path(aborted_here)).await
        ))
        .await;

        let changes = repair_dependency_failed(&db.connection(), evaluation)
            .await
            .unwrap();
        let mut statuses = Vec::new();
        for key in [
            aborted_here,
            parent_here,
            aborted_elsewhere,
            parent_elsewhere,
        ] {
            statuses.push(db.shared_build(&path(key)).await.status);
        }
        db.finish().await;

        assert_eq!(
            statuses,
            [
                BuildStatus::Aborted,
                BuildStatus::DependencyFailed,
                BuildStatus::Aborted,
                BuildStatus::Created
            ]
        );
        assert_eq!(changes.len(), 1);
    }
}
