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

/// Shared builds an evaluation found complete in our cache move from `Created` to
/// `Substituted`; a new shared build is inserted that way, this catches the ones a
/// prior evaluation left pending. Returns the transitions for the effects
/// emitter.
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

/// Repair shared build state from cache state across an evaluation's dependency
/// closure: any shared build whose outputs are **all** present in our cache
/// (`cached_path.file_hash`) is marked `Completed`, even if a
/// requeue / dependency-failed cascade / demote previously reset it. The dispatch
/// gate keys on the build-graph shared build state, which repeatedly desyncs from the
/// durable cache state - a derivation whose artifacts exist sits `Created` and
/// blocks its parents with nothing to build. Cache presence is the ground truth
/// for "is this built", so trust it here; the reactive heals
/// ([`crate::caches::demotion::demote_parents_of`] / [`crate::caches::demotion::demote_output_only_cached_deps`])
/// remain the backstop for the rare case where a cached output's runtime closure is
/// itself incomplete. Returns the changes it made, so the caller can advance the
/// parents of what it just settled; a shared build already terminal-success is left
/// alone, since it has nothing left for this statement to write.
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

    /// Cache presence is the ground truth for "built": a pending shared build whose
    /// outputs are complete in our cache is settled `Substituted` without a
    /// dispatch. Only `Created` moves, so a `Queued` shared build already in the
    /// tracker is not pulled out from under the dispatcher.
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

    /// A passthrough out on a worker settles its own shared build `Substituted`; the repair
    /// finding its outputs first called it built.
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
