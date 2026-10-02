/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::walks::eval_closure_cte;
use crate::status::TransitionChange;

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
        substituted = crate::sql::status::build(BuildStatus::Substituted),
        created = crate::sql::status::build(BuildStatus::Created),
    )
}

crate::sql_fn! {
    SUBSTITUTE_CREATED_SHARED_BUILDS = substitute_created_shared_builds_sql,
        params = [DerivationIds(64)];
}

pub async fn repair_cached_shared_builds_for_eval<C>(
    db: &C,
    evaluation: gradient_types::EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db).await?;
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
        LEFT JOIN cached_path cp ON cp.hash = o.hash AND cp.file_hash IS NOT NULL
        WHERE o.derivation = db.derivation AND cp.hash IS NULL)
    RETURNING db.derivation, old.status AS from_status, db.status AS to_status
    "#,
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
        completed = crate::sql::status::build(BuildStatus::Completed),
    )
}

crate::sql_fn! {
    REPAIR_CACHED_SHARED_BUILDS_FOR_EVAL = repair_cached_shared_builds_for_eval_sql,
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(500_000)
            .because("the same whole-closure walk PROMOTE_CLOSURE_QUERY pays for, plus \
                      one output-and-path anti-join per shared build the closure names"),
        flags = [Walk];
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn substitute_created_shared_builds_moves_only_created_rows() {
        let sql = substitute_created_shared_builds_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(sql.contains(&format!(
            "SET status = {}",
            crate::sql::status::build(BuildStatus::Substituted)
        )));
        assert!(sql.contains(&format!(
            "db.status = {}",
            crate::sql::status::build(BuildStatus::Created)
        )));
        assert!(sql.contains(
            "RETURNING db.derivation, old.status AS from_status, db.status AS to_status"
        ));
    }

    #[test]
    fn the_cached_repair_leaves_a_shared_build_whose_assignment_row_is_open() {
        let norm = |s: String| s.split_whitespace().collect::<Vec<_>>().join(" ");
        let gate = norm(
            crate::scheduling::assignment_record::no_open_assignment_predicate(
                &crate::scheduling::assignment_record::build_job_key_sql("db.id"),
            ),
        );

        assert!(norm(repair_cached_shared_builds_for_eval_sql()).contains(&gate));
    }
}
