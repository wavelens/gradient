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

/// `Created` to `Queued` for an explicit candidate list.
static PROMOTE: LazyLock<String> =
    LazyLock::new(|| promote_sql("db.derivation = ANY($1::uuid[]) AND "));

crate::sql_lazy! {
    PROMOTE_QUERY = || PROMOTE.as_str(),
        params = [DerivationIds(64)];
}

/// `Created` to `Queued` table-wide. The leading conjunct is implied by the gate (a
/// passthrough takes the `cache_available` arm, a build the `blocking_deps = 0` one) and is
/// written out anyway: it is the predicate of `idx-derivation_build-promotable`, and
/// spelling it makes the implication syntactic.
///
/// It is still the sweep's, not the queue's: the selective term is the gate's
/// `build_job` EXISTS, so the planner rightly drives from the jobs and reaches the
/// shared builds through that index rather than scanning it. [`repair_can_start`] is the
/// only caller and it covers the whole table by design.
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

/// The queue's own backstop: every `Queued` shared build whose gates no longer hold.
///
/// Table-wide with no index-friendly bound - it scans the `Queued` rows and evaluates
/// three `EXISTS` plus the negated gate for each, unlike [`PROMOTE_ANY`], which
/// matches a partial index. It is the sweep's most expensive statement and its row
/// count belongs in whatever the sweep reports.
static UNPROMOTE_UNGATED: LazyLock<String> = LazyLock::new(|| unpromote_ungated_sql(""));

crate::sql_lazy! {
    pub(super) UNPROMOTE_UNGATED_QUERY = || UNPROMOTE_UNGATED.as_str(),
        params = [],
        tier = Sweep;
}

/// [`UNPROMOTE_UNGATED`] bounded to a candidate list, for an event that just
/// invalidated a known set of gates.
static UNPROMOTE_UNGATED_IN: LazyLock<String> =
    LazyLock::new(|| unpromote_ungated_sql("db.derivation = ANY($1::uuid[]) AND "));

crate::sql_lazy! {
    UNPROMOTE_UNGATED_IN_QUERY = || UNPROMOTE_UNGATED_IN.as_str(),
        params = [DerivationIds(64)];
}

/// Queue every `Created` candidate whose gates hold. The gate is embedded, so a
/// candidate list is a bound and never a claim: passing a row that cannot start yet
/// moves nothing.
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

/// Queue every promotable shared build in an evaluation's dependency closure. What a
/// finished walk happens once, so shared builds whose dependencies were already fetchable at
/// resolve time - for which no completion event ever fires - are seeded from the
/// closure instead of waiting for one.
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

/// Pull back every `Queued` shared build whose own `.drv` is among `drv_hashes`, and whose
/// gates the loss of that `.drv` closed. The full gate is embedded rather than the
/// `.drv` term alone, so this is [`unpromote_ungated`] scoped by hash: a `.drv`
/// re-pushed between the loss and this call keeps its shared build queued, and a passthrough,
/// whose arm of the gate never reads the `.drv`, is left alone.
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

/// Pull back every `Queued` candidate whose gates no longer hold. The gate is
/// embedded, so the candidate list is a bound and never a claim: a shared build a
/// concurrent evaluation re-walked between the event and this call keeps its place
/// in the queue.
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

/// Drop the record of `derivations` and close the gates that read `walked`, so the
/// next evaluation walks them again: the un-promoted shared builds come back as transitions
/// for the caller to fan out with [`crate::status::emit_transition_effects`].
///
/// The un-walk goes first, because it locks `derivation` rows before [`lock_shared_builds`]
/// and [`unpromote_ungated`] reach `derivation_build` - the class order record
/// (`upsert_walked`, then the shared build locks) and the GC's orphan reclaim both take. The
/// shared build pass that follows acquires the un-promoted rows in `derivation` order instead
/// of the one `unpromote_ungated`'s own UPDATE would pick.
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

    /// Promotion embeds the whole gate and moves Created rows only, so a candidate list
    /// bounds the statement without asserting anything about the rows in it.
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

    /// A `.drv` owner leaves the queue only while its gates really are shut, so a
    /// re-push between the loss and this call keeps its shared build queued - and a passthrough,
    /// whose arm of the gate never reads the `.drv`, is never pulled back by one.
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

    /// The generic un-promote is what every need loss executes, so it must move
    /// `Queued` rows only and report the move as the transition the emitter fans out.
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

    /// A dispatched job keeps its shared build `Queued` until the worker reports, so an
    /// un-promote that reached it would skip a build already running and reject
    /// the `Building` that follows.
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

    /// The un-walk selects what was complete, drops the record, and only then locks
    /// shared builds: derivation rows first, as every writer of them does.
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
