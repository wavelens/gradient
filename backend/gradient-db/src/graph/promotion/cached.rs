/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::predicates::content_variant_trusted_by_all_projects_with_live_job_on_shared_build;
use crate::graph::walks::eval_closure_cte;
use crate::sql::StatusWrite;
use crate::status::TransitionChange;
use crate::store_path_content_variant_trust::project_trusts_content_variant_exists_sql;

use super::transitions::returned_transitions;
use gradient_entity::build::BuildStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};

pub async fn substitute_created_shared_builds<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    let rows = db
        .query_all_raw(SUBSTITUTE_CREATED_SHARED_BUILDS.bind([ids.into()]))
        .await?;

    Ok(returned_transitions(rows))
}

fn substitute_created_shared_builds_sql() -> String {
    format!(
        r#"
        UPDATE derivation_build AS db
        SET status = {substituted}, substituted = true,
            updated_at = (now() AT TIME ZONE 'UTC')
        WHERE db.status = {created} AND db.derivation = ANY($1::uuid[])
        RETURNING db.derivation, old.status AS from_status, db.status AS to_status
        "#,
        substituted = crate::sql::status::to(BuildStatus::SUBSTITUTE_CREATED),
        created = crate::sql::status::from(BuildStatus::SUBSTITUTE_CREATED),
    )
}

crate::sql_fn! {
    SUBSTITUTE_CREATED_SHARED_BUILDS = substitute_created_shared_builds_sql,
        params = [DerivationIds(64)],
        status_write = StatusWrite::build(BuildStatus::SUBSTITUTE_CREATED);
}

pub async fn repair_cached_shared_builds_for_eval<C>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db, &REPAIR_CACHED_SHARED_BUILDS_FOR_EVAL).await?;
    let rows = walk
        .query_all_raw(
            REPAIR_CACHED_SHARED_BUILDS_FOR_EVAL.bind([Value::Uuid(Some(evaluation.into_inner()))]),
        )
        .await?;
    walk.commit().await?;

    Ok(returned_transitions(rows))
}

fn repair_cached_shared_builds_for_eval_sql() -> String {
    let cte = eval_closure_cte();
    let not_in_flight = crate::scheduling::assignment_record::no_open_assignment_predicate(
        &crate::scheduling::assignment_record::build_job_key_sql("db.id"),
    );
    format!(
        r#"
    {cte}
    UPDATE derivation_build db
    SET status = CASE WHEN db.status IN ({terminal_success}) THEN db.status ELSE {completed} END,
        updated_at = (now() AT TIME ZONE 'UTC')
    WHERE db.derivation IN (SELECT derivation FROM closure)
      AND db.status NOT IN ({terminal_success})
      AND {not_in_flight}
      AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation)
      AND NOT EXISTS (
        SELECT 1 FROM derivation_output o
        WHERE o.derivation = db.derivation
          AND NOT EXISTS (
            SELECT 1 FROM cached_path cp
            WHERE cp.hash = o.hash
              AND {evaluation_project_trusts} AND {live_projects_trust}))
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#,
        evaluation_project_trusts = project_trusts_content_variant_exists_sql(
            "cp.id",
            "(SELECT repair_task.project FROM evaluation repair_evaluation \
              JOIN task repair_task ON repair_task.id = repair_evaluation.task \
              WHERE repair_evaluation.id = $1)",
        ),
        live_projects_trust =
            content_variant_trusted_by_all_projects_with_live_job_on_shared_build("cp.id", "db"),
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
        completed = crate::sql::status::to(BuildStatus::COMPLETE_CACHED),
    )
}

crate::sql_fn! {
    REPAIR_CACHED_SHARED_BUILDS_FOR_EVAL = repair_cached_shared_builds_for_eval_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(750_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, plus \
                      one output-and-path anti-join per unfinished shared build the closure \
                      names; partly substituted producers stay unfinished, observed 567_380"),
        flags = [Walk],
        status_write = StatusWrite::build(BuildStatus::COMPLETE_CACHED);
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::EvaluationId;

    fn path(key: &str) -> String {
        format!("/nix/store/{}-cached.drv", key.repeat(32))
    }

    #[tokio::test]
    async fn test_substitute_created_shared_builds_moves_only_created_builds_to_substituted() {
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&path("1"))
            .derivation(&path("2"))
            .build_status(BuildStatus::Queued)
            .derivation(&path("3"))
            .into_database()
            .await;
        let (created, queued) = (db.id_of(&path("1")).await, db.id_of(&path("2")).await);

        let changes = substitute_created_shared_builds(&db.connection(), &[created, queued])
            .await
            .unwrap();
        let rows = [
            db.shared_build(&path("1")).await,
            db.shared_build(&path("2")).await,
            db.shared_build(&path("3")).await,
        ];
        db.finish().await;

        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].derivation, changes[0].from, changes[0].to),
            (created, BuildStatus::Created, BuildStatus::Substituted)
        );
        assert_eq!(
            rows.map(|row| (row.status, row.substituted)),
            [
                (BuildStatus::Substituted, true),
                (BuildStatus::Queued, false),
                (BuildStatus::Created, false),
            ]
        );
    }

    #[tokio::test]
    async fn repair_cached_shared_builds_for_eval_only_completes_builds_with_all_outputs_stored() {
        use gradient_entity::build_attempt::AttemptOutcome;

        let (evaluation, other_evaluation) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let stored = |shape: gradient_fixtures::shape::Shape, key: &str, status: BuildStatus| {
            shape
                .derivation(&path(key))
                .output(&key.repeat(32))
                .build_status(status)
                .job()
                .cached_path(&key.repeat(32), key)
        };
        let shape = gradient_fixtures::shape::shape().evaluation(evaluation);
        let shape = stored(shape, "1", BuildStatus::Created);
        let shape = stored(shape, "2", BuildStatus::Queued);
        let shape = stored(shape, "4", BuildStatus::Completed);
        let db = shape
            .derivation(&path("3"))
            .output(&"3".repeat(32))
            .job()
            .derivation(&path("5"))
            .output(&"5".repeat(32))
            .build_status(BuildStatus::Building)
            .job()
            .attempt(AttemptOutcome::Running, None)
            .open_assignment()
            .cached_path(&"5".repeat(32), "5")
            .derivation(&path("6"))
            .job()
            .evaluation(other_evaluation)
            .derivation(&path("7"))
            .output(&"7".repeat(32))
            .job()
            .cached_path(&"7".repeat(32), "7")
            .into_database()
            .await;

        let changes = repair_cached_shared_builds_for_eval(&db.connection(), evaluation)
            .await
            .unwrap();
        let mut statuses = Vec::new();
        for key in ["1", "2", "3", "4", "5", "6", "7"] {
            statuses.push(db.shared_build(&path(key)).await.status);
        }
        db.finish().await;

        assert_eq!(changes.len(), 2);
        assert_eq!(
            statuses,
            [
                BuildStatus::Completed,
                BuildStatus::Completed,
                BuildStatus::Created,
                BuildStatus::Completed,
                BuildStatus::Building,
                BuildStatus::Created,
                BuildStatus::Created,
            ]
        );
    }
}
