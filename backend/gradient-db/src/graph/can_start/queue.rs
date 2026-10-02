/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::promotion::{returned_derivations, returned_transitions, transitions_from};
use crate::graph::{
    predicates::{gates_predicate, promotable_predicate},
    walks::eval_closure_cte,
};
use crate::status::TransitionChange;

use super::lock::{ids, lock_shared_builds};
use gradient_entity::build::BuildStatus;
use gradient_types::{DerivationId, EvaluationId};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};
use std::sync::LazyLock;

fn promote_sql(scope: &str) -> String {
    format!(
        "UPDATE derivation_build db \
         SET status = {queued}, queued_at = coalesce(db.queued_at, now() AT TIME ZONE 'UTC'), \
             updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE {scope}{promotable} \
         RETURNING db.derivation",
        queued = crate::sql::status::build(BuildStatus::Queued),
        promotable = promotable_predicate("db"),
    )
}

static PROMOTE: LazyLock<String> =
    LazyLock::new(|| promote_sql("db.derivation = ANY($1::uuid[]) AND "));

crate::sql_lazy! {
    PROMOTE_QUERY = || PROMOTE.as_str(),
        params = [DerivationIds(64)];
}

static PROMOTE_ANY: LazyLock<String> =
    LazyLock::new(|| promote_sql("(db.blocking_deps = 0 OR db.cache_available) AND "));

crate::sql_lazy! {
    pub(super) PROMOTE_ANY_QUERY = || PROMOTE_ANY.as_str(),
        params = [],
        tier = Sweep;
}

static PROMOTE_CLOSURE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{cte} {promote}",
        cte = eval_closure_cte(),
        promote = promote_sql("db.derivation IN (SELECT derivation FROM closure) AND "),
    )
});

crate::sql_lazy! {
    PROMOTE_CLOSURE_QUERY = || PROMOTE_CLOSURE.as_str(),
        params = [EvaluationId],
        tier = Walk,
        budget = crate::sql::Budget::walk().buffers(900_000)
            .because("the scope is an evaluation's whole dependency closure, and the \
                      walk that names it reads ~5 buffers per node it visits"),
        flags = [Walk];
}

fn unpromote_sql(reason: &str) -> String {
    format!(
        "UPDATE derivation_build db \
         SET status = {created}, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.status = {queued} AND {not_in_flight} AND {reason} \
         RETURNING db.derivation, old.status AS from_status, db.status AS to_status",
        created = crate::sql::status::build(BuildStatus::Created),
        queued = crate::sql::status::build(BuildStatus::Queued),
        not_in_flight = crate::scheduling::assignment_record::no_open_assignment_predicate(
            &crate::scheduling::assignment_record::build_job_key_sql("db.id")
        ),
    )
}

fn unpromote_ungated_sql(scope: &str) -> String {
    unpromote_sql(&format!(
        "{scope}NOT {gates}",
        gates = gates_predicate("db")
    ))
}

static UNPROMOTE_DRV_OWNERS: LazyLock<String> = LazyLock::new(|| {
    unpromote_ungated_sql(
        "EXISTS (SELECT 1 FROM derivation d \
                 WHERE d.id = db.derivation AND d.hash = ANY($1::text[])) AND ",
    )
});

crate::sql_lazy! {
    UNPROMOTE_DRV_OWNERS_QUERY = || UNPROMOTE_DRV_OWNERS.as_str(),
        params = [DerivationHashes(64)];
}

static UNPROMOTE_UNGATED: LazyLock<String> = LazyLock::new(|| unpromote_ungated_sql(""));

crate::sql_lazy! {
    pub(super) UNPROMOTE_UNGATED_QUERY = || UNPROMOTE_UNGATED.as_str(),
        params = [],
        tier = Sweep;
}

static UNPROMOTE_UNGATED_IN: LazyLock<String> =
    LazyLock::new(|| unpromote_ungated_sql("db.derivation = ANY($1::uuid[]) AND "));

crate::sql_lazy! {
    UNPROMOTE_UNGATED_IN_QUERY = || UNPROMOTE_UNGATED_IN.as_str(),
        params = [DerivationIds(64)];
}

pub async fn promote<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(PROMOTE_QUERY.bind([ids(candidates)]))
        .await?;

    Ok(transitions_from(
        returned_derivations(rows),
        BuildStatus::Created,
        BuildStatus::Queued,
    ))
}

pub async fn promote_closure<C>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db).await?;
    let rows = walk
        .query_all_raw(PROMOTE_CLOSURE_QUERY.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?;
    walk.commit().await?;

    Ok(transitions_from(
        returned_derivations(rows),
        BuildStatus::Created,
        BuildStatus::Queued,
    ))
}

pub async fn unpromote_drv_owners<C: ConnectionTrait>(
    db: &C,
    drv_hashes: &[String],
) -> Result<Vec<TransitionChange>, DbErr> {
    if drv_hashes.is_empty() {
        return Ok(Vec::new());
    }

    Ok(returned_transitions(
        db.query_all_raw(UNPROMOTE_DRV_OWNERS_QUERY.bind([drv_hashes.to_vec().into()]))
            .await?,
    ))
}

/// The gate is embedded, and the candidate list is a bound, never a claim.
/// A shared build re-walked by a concurrent evaluation is keeping its place in the queue.
pub async fn unpromote_ungated<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    Ok(returned_transitions(
        db.query_all_raw(UNPROMOTE_UNGATED_IN_QUERY.bind([ids(candidates)]))
            .await?,
    ))
}

/// The un-walk must go first and lock `derivation` rows before `derivation_build`.
/// Record and the GC's orphan reclaim are following the same class order.
pub async fn unwalk_derivations(
    ctx: &crate::DbContext,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let txn = ctx.worker_db.begin().await?;
    crate::graph::walk_completeness::unwalk(&txn, derivations).await?;
    let _shared_builds = lock_shared_builds(&txn, derivations).await?;
    let changes = unpromote_ungated(&txn, derivations).await?;
    txn.commit().await?;

    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::can_start::test_rows::{exec, norm, transition_row};
    use crate::pool::statements;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    #[test]
    fn promote_embeds_the_promotable_predicate() {
        for sql in [norm(&PROMOTE), norm(&PROMOTE_ANY)] {
            assert!(sql.contains("SET status = 1"), "{sql}");
            assert!(sql.contains(&norm(&promotable_predicate("db"))), "{sql}");
            assert!(sql.contains("RETURNING db.derivation"), "{sql}");
            assert!(
                !sql.contains("dispatched_job"),
                "promotion is a can-start decision, not a dispatch one: {sql}"
            );
        }

        assert!(norm(&PROMOTE).contains("db.derivation = ANY($1::uuid[])"));
        assert!(
            norm(&PROMOTE_ANY).contains("(db.blocking_deps = 0 OR db.cache_available) AND"),
            "the table-wide promote must spell out its partial-index bound: {}",
            norm(&PROMOTE_ANY)
        );

        let closure = norm(&PROMOTE_CLOSURE);
        assert!(
            closure.starts_with("WITH RECURSIVE closure(derivation) AS"),
            "{closure}"
        );
        assert!(
            closure.contains("db.derivation IN (SELECT derivation FROM closure)"),
            "{closure}"
        );
        assert!(
            closure.contains(&norm(&promotable_predicate("db"))),
            "{closure}"
        );
    }

    #[test]
    fn unpromoting_a_drv_owner_rechecks_the_complete_gate() {
        let sql = norm(&UNPROMOTE_DRV_OWNERS);
        assert!(sql.contains("d.hash = ANY($1::text[])"), "{sql}");
        assert!(
            sql.contains(&format!("AND NOT {}", norm(&gates_predicate("db")))),
            "{sql}"
        );
        assert!(sql.contains("db.status = 1"), "queued rows only: {sql}");
        assert!(
            sql.contains("RETURNING db.derivation, old.status AS from_status"),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn unpromote_ungated_moves_queued_rows_back_to_created() {
        let d = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![transition_row(
                d,
                i32::from(BuildStatus::Queued),
                i32::from(BuildStatus::Created),
            )]])
            .into_connection();

        let changes = unpromote_ungated(&db, &[d]).await.unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Queued, BuildStatus::Created)
        );
        let log = statements(db.into_transaction_log());
        assert!(log[0].contains("SET status = 0"), "{log:?}");
        assert!(
            log[0].contains("db.derivation = ANY($1::uuid[]) AND NOT ("),
            "{log:?}"
        );
        assert!(
            log[0].contains("AND db.wanted"),
            "the embedded gate reads the need column: {log:?}"
        );
    }

    #[test]
    fn no_unpromote_takes_a_shared_build_out_from_under_its_job() {
        let gate = norm(
            &crate::scheduling::assignment_record::no_open_assignment_predicate(
                &crate::scheduling::assignment_record::build_job_key_sql("db.id"),
            ),
        );
        for sql in [
            &*UNPROMOTE_UNGATED,
            &*UNPROMOTE_UNGATED_IN,
            &*UNPROMOTE_DRV_OWNERS,
        ] {
            assert!(norm(sql).contains(&gate), "{sql}");
        }
    }

    #[tokio::test]
    async fn an_unwalk_moves_the_counter_before_it_reaches_the_shared_builds() {
        let gone = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1), exec(0)])
            .append_query_results([vec![BTreeMap::from([(
                "id".to_owned(),
                Value::from(gone.into_inner()),
            )])]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx(db).await;

        unwalk_derivations(&ctx, &[gone]).await.unwrap();
        drop(ctx);

        let log = crate::pool::raw_statements(pool.into_transaction_log());
        let at = |needle: &str| {
            log.iter()
                .position(|s| s.sql.contains(needle))
                .unwrap_or_else(|| panic!("{needle} must run: {log:?}"))
        };
        let complete = at("AND walked AND unwalked_inputs = 0 ORDER BY id FOR NO KEY UPDATE");
        let unwalk = at("UPDATE derivation SET walked = false");
        let shared_builds = at("FROM derivation_build");
        assert!(complete < unwalk && unwalk < shared_builds, "{log:?}");
    }
}
