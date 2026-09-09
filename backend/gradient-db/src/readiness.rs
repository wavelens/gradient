/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `derivation_build.fetchable` and `derivation_build.unready_deps`: whether an
//! anchor can serve its outputs to a dependent, and how many of an anchor's direct
//! dependencies cannot. Zero is the readiness gate, so promotion and dispatch read
//! one integer per row where they used to walk the build graph, and nothing in this
//! module recurses over `derivation_dependency`.
//!
//! Every flip of `fetchable` is written by a statement whose `RETURNING` names
//! exactly the anchors that changed, the direct dependents' counter moves from that
//! set in ONE update, and the dependents that reached zero are promoted.
//! [`crate::graph_sql::fetchable_predicate`] is the one definition of the flag and
//! [`crate::graph_sql::gates_predicate`] the one definition of the gate.
//!
//! Like `cached_path.missing_references`, this counter is MOVED and not derived, so
//! every ripple must be driven by a TRANSITION and never by a state: rippling from a
//! row that did not just flip, or rippling one frontier twice, moves a dependent
//! past zero, and a negative counter never satisfies `= 0` again. [`became_fetchable`]
//! and [`lost_fetchability`] therefore mark first and ripple only from the rows
//! their own `RETURNING` reports. `NOT db.fetchable` (respectively `db.fetchable`)
//! is a column of the row being updated, so Postgres re-checks it under EvalPlanQual
//! after any lock wait and two concurrent markers cannot both claim one flip.
//!
//! # One level, not a fixpoint
//!
//! The NAR ripple recurses because wholeness is transitive: a path that becomes
//! whole makes its referrer whole. Readiness is not. A dependent that reaches zero
//! becomes QUEUED, not fetchable - only a finished build or an upstream copy makes
//! an anchor fetchable - so the frontier stops at the direct dependents and one
//! statement per flip is the entire ripple.
//!
//! # The seed and the repair are two halves of one invariant
//!
//! Both compose [`unready_dependency_count`], where a dependency with NO anchor row
//! counts as unready. The inner join that would drop such an edge fails OPEN, in a
//! gate whose entire job is to stop a dispatch against a missing input.
//! [`seed_unready_deps`] writes the count ABSOLUTELY, never adjusting what it finds,
//! so it doubles as a repair of one row; it must run in the transaction that wrote
//! the edges it counts, because a ripple that crosses a seed - the edge visible to
//! the ripple, the dependency already fetchable when the seed counted it - leaves
//! the dependent one below its true value.
//!
//! # No row locks, and why the drift is recoverable anyway
//!
//! Nothing here locks `derivation_build`. The marks are exact by the EvalPlanQual
//! re-check above, and both ripples move the counter relative to the row's own
//! value, so they compose with a concurrent move exactly as `nar_closure`'s do.
//! What stays unserialised is a concurrent edge insert: a ripple's edge set and a
//! seed's count are read under two different snapshots, and no lock on
//! `derivation_build` closes a race whose subject is a `derivation_dependency` row.
//!
//! That is affordable here in a way it is not for the NAR counter, because
//! [`repair_pending`] recomputes ABSOLUTELY over every pending anchor and its direct
//! dependencies, which is precisely the set the gates read: `unready_deps` is read
//! only by [`crate::graph_sql::gates_predicate`], applied only to `Created` and
//! `Queued` rows, and `fetchable` is read only one edge away from such a row. So no
//! value a gate can act on has an unrecoverable state, negative counters included -
//! unlike `nar_closure::repair_counters_for`, whose bound leaves a path outside the
//! gating set unwhole forever. Two drifted levels still take two sweeps: the repair
//! recounts from one snapshot and ripples nothing, or it would drive the dependents
//! of a row it just corrected past zero.
//!
//! An anchor's own queue membership is not a function of its own `fetchable`, so
//! neither flip touches it. The one event that moves both is `substitutable` being
//! cleared on a `Queued` anchor, and [`repair_pending`]'s un-promote pass is what
//! settles that.

use crate::graph_sql::{
    drv_whole_predicate, eval_closure_cte, fetchable_predicate, gates_predicate,
    promotable_predicate,
};
use crate::promotion::{returned_derivations, returned_transitions, transitions_from};
use crate::status::TransitionChange;
use crate::status_sql;
use gradient_entity::build::BuildStatus;
use gradient_types::{DerivationId, EvaluationId};
use sea_orm::{ConnectionTrait, DatabaseBackend, DbErr, Statement, Value};
use std::sync::LazyLock;

/// The statuses whose readiness any gate can act on. A terminal anchor's counter
/// is read by nothing, which is what bounds [`repair_pending`].
const PENDING: [BuildStatus; 2] = [BuildStatus::Created, BuildStatus::Queued];

/// The value `unready_deps` holds for anchor `{alias}`: its direct dependencies
/// whose anchor row is absent, or present and not fetchable. The seed and the
/// repair are two halves of one invariant - if their counts ever disagree, every
/// sweep rewrites correct rows into incorrect ones - so both compose this.
///
/// The count is per EDGE, and both ripples move a dependent by `count(*)` over the
/// same edge rows, so a duplicated edge is counted twice and cancelled twice.
/// Self-edges are NOT excluded, unlike `nar_closure`'s reference count: a store
/// path routinely references itself, a derivation cannot be its own input, and
/// excluding them here would diverge from the frozen backfill for nothing.
fn unready_dependency_count(alias: &str) -> String {
    format!(
        "(SELECT count(*) FROM derivation_dependency e \
         LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
         WHERE e.derivation = {alias}.derivation \
           AND (dep.derivation IS NULL OR NOT dep.fetchable))"
    )
}

static SEED_UNREADY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET unready_deps = {count} \
         WHERE db.derivation = ANY($1::uuid[])",
        count = unready_dependency_count("db"),
    )
});

static MARK_FETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = true \
         WHERE db.derivation = ANY($1::uuid[]) AND NOT db.fetchable AND {pred} \
         RETURNING db.derivation",
        pred = fetchable_predicate("db"),
    )
});

static MARK_UNFETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = false \
         WHERE db.derivation = ANY($1::uuid[]) AND db.fetchable AND NOT {pred} \
         RETURNING db.derivation",
        pred = fetchable_predicate("db"),
    )
});

const RIPPLE_DOWN: &str = r#"
    UPDATE derivation_build d
    SET unready_deps = d.unready_deps - c.n
    FROM (SELECT e.derivation, count(*) AS n FROM derivation_dependency e
          WHERE e.dependency = ANY($1::uuid[]) GROUP BY e.derivation) c
    WHERE d.derivation = c.derivation
    RETURNING d.derivation, d.unready_deps = 0 AS ready
"#;

static RIPPLE_UP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build d \
         SET unready_deps = d.unready_deps + c.n, \
             status = CASE WHEN d.status = {queued} THEN {created} ELSE d.status END, \
             updated_at = (now() AT TIME ZONE 'UTC') \
         FROM derivation_build old, \
              (SELECT e.derivation, count(*) AS n FROM derivation_dependency e \
               WHERE e.dependency = ANY($1::uuid[]) GROUP BY e.derivation) c \
         WHERE d.derivation = c.derivation AND old.id = d.id \
         RETURNING d.derivation, old.status AS from_status, d.status AS to_status",
        queued = status_sql::build(BuildStatus::Queued),
        created = status_sql::build(BuildStatus::Created),
    )
});

fn promote_sql(scope: &str) -> String {
    format!(
        "UPDATE derivation_build db \
         SET status = {queued}, queued_at = coalesce(db.queued_at, now() AT TIME ZONE 'UTC'), \
             updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE {scope}{promotable} \
         RETURNING db.derivation",
        queued = status_sql::build(BuildStatus::Queued),
        promotable = promotable_predicate("db"),
    )
}

/// `Created` to `Queued` for an explicit candidate list.
static PROMOTE: LazyLock<String> =
    LazyLock::new(|| promote_sql("db.derivation = ANY($1::uuid[]) AND "));

/// `Created` to `Queued` table-wide. Bounded by `idx-derivation_build-promotable`
/// (`status = 0 AND unready_deps = 0`), so this is a partial-index lookup and not a
/// table pass.
static PROMOTE_ANY: LazyLock<String> = LazyLock::new(|| promote_sql(""));

static PROMOTE_CLOSURE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{cte} {promote}",
        cte = eval_closure_cte(),
        promote = promote_sql("db.derivation IN (SELECT derivation FROM closure) AND "),
    )
});

fn unpromote_sql(reason: &str) -> String {
    format!(
        "UPDATE derivation_build db \
         SET status = {created}, updated_at = (now() AT TIME ZONE 'UTC') \
         FROM derivation_build old \
         WHERE old.id = db.id AND db.status = {queued} AND {reason} \
         RETURNING db.derivation, old.status AS from_status, db.status AS to_status",
        created = status_sql::build(BuildStatus::Created),
        queued = status_sql::build(BuildStatus::Queued),
    )
}

static UNPROMOTE_DRV_OWNERS: LazyLock<String> = LazyLock::new(|| {
    unpromote_sql(&format!(
        "EXISTS (SELECT 1 FROM derivation d \
                 WHERE d.id = db.derivation AND d.hash = ANY($1::text[])) \
         AND NOT (db.substitutable OR {drv_whole})",
        drv_whole = drv_whole_predicate("db"),
    ))
});

static UNPROMOTE_UNGATED: LazyLock<String> =
    LazyLock::new(|| unpromote_sql(&format!("NOT {gates}", gates = gates_predicate("db"))));

/// The pending anchors and their direct dependencies: every row whose `fetchable`
/// a gate can read this pass, one edge deep.
fn repair_scope() -> String {
    let pending = status_sql::build_in(&PENDING);
    format!(
        "SELECT q.derivation FROM derivation_build q WHERE q.status IN ({pending}) \
       UNION \
         SELECT e.dependency FROM derivation_dependency e \
         JOIN derivation_build q ON q.derivation = e.derivation \
         WHERE q.status IN ({pending})"
    )
}

static REPAIR_FETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = x.f \
         FROM (SELECT p.derivation, p.fetchable AS old, {pred} AS f \
               FROM derivation_build p WHERE p.derivation IN ({scope})) x \
         WHERE db.derivation = x.derivation AND db.fetchable = x.old AND x.old <> x.f",
        pred = fetchable_predicate("p"),
        scope = repair_scope(),
    )
});

static REPAIR_UNREADY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET unready_deps = x.n \
         FROM (SELECT p.derivation, p.unready_deps AS old, {count} AS n \
               FROM derivation_build p WHERE p.status IN ({pending})) x \
         WHERE db.derivation = x.derivation AND db.unready_deps = x.old AND x.old <> x.n",
        count = unready_dependency_count("p"),
        pending = status_sql::build_in(&PENDING),
    )
});

fn ids(derivations: &[DerivationId]) -> Value {
    derivations
        .iter()
        .map(|d| d.into_inner())
        .collect::<Vec<uuid::Uuid>>()
        .into()
}

/// Recount, for each of `derivations`, the direct dependencies that cannot yet
/// serve their outputs, and write it absolutely. Returns the rows written.
///
/// Run it for a derivation whose edges just landed, in the transaction that wrote
/// them: the count is over `derivation_dependency`, so an edge inserted after the
/// seed is one a later ripple can cancel without it ever having been counted. It
/// overwrites rather than adjusts on purpose - the value it replaces may be the
/// migration's backfill, which inner-joined the dependency anchor and so counted a
/// dependency with no anchor row as ready.
pub async fn seed_unready_deps<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<u64, DbErr> {
    if derivations.is_empty() {
        return Ok(0);
    }

    Ok(db
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            SEED_UNREADY.as_str(),
            [ids(derivations)],
        ))
        .await?
        .rows_affected())
}

async fn mark<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
    to: bool,
) -> Result<Vec<DerivationId>, DbErr> {
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let statement = if to {
        MARK_FETCHABLE.as_str()
    } else {
        MARK_UNFETCHABLE.as_str()
    };

    Ok(returned_derivations(
        db.query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            statement,
            [ids(derivations)],
        ))
        .await?,
    ))
}

/// Flip `derivations` to fetchable where the predicate now holds, decrement their
/// direct dependents' counters, and queue the dependents that reached zero.
///
/// The returned transitions are `Created` to `Queued`, and the ripple runs only
/// over the rows the mark actually flipped: a caller that hands over an anchor that
/// was already fetchable gets no statement past the mark, which is what keeps a
/// dependent's counter from going below zero.
pub async fn became_fetchable<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let flipped = mark(db, derivations, true).await?;
    if flipped.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            RIPPLE_DOWN,
            [ids(&flipped)],
        ))
        .await?;

    let mut ready = Vec::new();
    for row in rows {
        if row.try_get::<bool>("", "ready")? {
            ready.push(DerivationId::new(
                row.try_get::<uuid::Uuid>("", "derivation")?,
            ));
        }
    }

    promote(db, &ready).await
}

/// Flip `derivations` to not fetchable where the predicate no longer holds,
/// increment their direct dependents' counters, and pull the queued dependents back
/// to `Created`. The returned transitions are `Queued` to `Created`; a dependent
/// that was `Created`, `Building` or terminal only counts up.
pub async fn lost_fetchability<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let flipped = mark(db, derivations, false).await?;
    if flipped.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            RIPPLE_UP.as_str(),
            [ids(&flipped)],
        ))
        .await?;

    Ok(returned_transitions(rows)
        .into_iter()
        .filter(|c| c.from != c.to)
        .collect())
}

/// Queue every `Created` candidate whose gates hold. The gate is embedded, so a
/// candidate list is a bound and never a claim: passing a row that is not yet ready
/// moves nothing.
pub async fn promote<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            PROMOTE.as_str(),
            [ids(candidates)],
        ))
        .await?;

    Ok(transitions_from(
        returned_derivations(rows),
        BuildStatus::Created,
        BuildStatus::Queued,
    ))
}

/// Queue every promotable anchor in an evaluation's dependency closure. What a
/// finished walk runs once, so anchors whose dependencies were already fetchable at
/// resolve time - for which no completion event ever fires - are seeded from the
/// closure instead of waiting for one.
pub async fn promote_closure<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            PROMOTE_CLOSURE.as_str(),
            [Value::Uuid(Some(evaluation.into_inner()))],
        ))
        .await?;

    Ok(transitions_from(
        returned_derivations(rows),
        BuildStatus::Created,
        BuildStatus::Queued,
    ))
}

/// A `.drv` that is no longer whole cannot be imported, so its owner leaves the
/// queue unless an upstream serves it. The statement re-checks that ground truth
/// rather than trusting the hash list: a `.drv` re-pushed between the loss and this
/// call must keep its anchor queued.
pub async fn unpromote_drv_owners<C: ConnectionTrait>(
    db: &C,
    drv_hashes: &[String],
) -> Result<Vec<TransitionChange>, DbErr> {
    if drv_hashes.is_empty() {
        return Ok(Vec::new());
    }

    Ok(returned_transitions(
        db.query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            UNPROMOTE_DRV_OWNERS.as_str(),
            [drv_hashes.to_vec().into()],
        ))
        .await?,
    ))
}

/// What the consistency sweep repaired.
#[derive(Debug, Default)]
pub struct Repaired {
    pub fetchable: u64,
    pub unready_deps: u64,
    pub promoted: Vec<TransitionChange>,
    pub unpromoted: Vec<TransitionChange>,
}

/// Recompute `fetchable` over the pending anchors and their direct dependencies,
/// then `unready_deps` over the pending anchors, write only what differs, and settle
/// the queue against the gates.
///
/// `fetchable` first, so the recount reads repaired inputs; the counter recompute is
/// a separate statement and therefore a fresh snapshot. Each write is a
/// compare-and-swap on the pre-image: without a row lock the fresh row is compared
/// against a value the statement's own snapshot produced, so a caller racing a mark
/// degrades to a skipped row instead of overwriting a counter nothing else
/// re-derives, and `old <> new` is the drift filter behind the returned counts.
///
/// The un-promote runs before the promote, and they cannot both move a row because
/// [`gates_predicate`] never reads `status`. Bounded by the pending set: drift on a
/// terminal anchor surfaces only through a pending dependent, which is exactly as
/// far as any gate reads.
pub async fn repair_pending<C: ConnectionTrait>(db: &C) -> Result<Repaired, DbErr> {
    let fetchable = db
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            REPAIR_FETCHABLE.as_str().to_owned(),
        ))
        .await?
        .rows_affected();
    let unready_deps = db
        .execute_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            REPAIR_UNREADY.as_str().to_owned(),
        ))
        .await?
        .rows_affected();
    let unpromoted = returned_transitions(
        db.query_all_raw(Statement::from_string(
            DatabaseBackend::Postgres,
            UNPROMOTE_UNGATED.as_str().to_owned(),
        ))
        .await?,
    );
    let promoted = transitions_from(
        returned_derivations(
            db.query_all_raw(Statement::from_string(
                DatabaseBackend::Postgres,
                PROMOTE_ANY.as_str().to_owned(),
            ))
            .await?,
        ),
        BuildStatus::Created,
        BuildStatus::Queued,
    );

    Ok(Repaired {
        fetchable,
        unready_deps,
        promoted,
        unpromoted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pool::statements;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn drv(id: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([("derivation".to_owned(), Value::from(id.into_inner()))])
    }

    fn ripple_row(id: DerivationId, ready: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("ready".to_owned(), Value::from(ready)),
        ])
    }

    fn transition_row(id: DerivationId, from: i32, to: i32) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("from_status".to_owned(), Value::from(from)),
            ("to_status".to_owned(), Value::from(to)),
        ])
    }

    fn exec(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    /// A dependency with no anchor row at all must count as UNREADY. The
    /// migration's backfill inner-joined it and so counted zero, failing open in a
    /// gate whose whole job is to stop a dispatch against a missing input.
    #[test]
    fn the_count_treats_a_dependency_with_no_anchor_as_unready() {
        let sql = norm(&unready_dependency_count("db"));
        assert_eq!(
            sql,
            "(SELECT count(*) FROM derivation_dependency e \
             LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
             WHERE e.derivation = db.derivation \
             AND (dep.derivation IS NULL OR NOT dep.fetchable))"
        );
    }

    /// The seed writes the count absolutely, so it never accumulates onto - or
    /// trusts - the value it finds, and a leaf gets zero from an empty count.
    #[test]
    fn the_seed_overwrites_and_never_adjusts() {
        let sql = norm(&SEED_UNREADY);
        assert!(
            sql.starts_with(&format!(
                "UPDATE derivation_build db SET unready_deps = {}",
                norm(&unready_dependency_count("db"))
            )),
            "{sql}"
        );
        assert!(
            !sql.contains("unready_deps + ") && !sql.contains("unready_deps - "),
            "the seed is absolute, not an adjustment: {sql}"
        );
        assert!(
            sql.contains("WHERE db.derivation = ANY($1::uuid[])"),
            "{sql}"
        );
    }

    /// An empty batch is not a statement: every entry point short-circuits so a
    /// caller can hand over whatever its event produced.
    #[tokio::test]
    async fn an_empty_batch_touches_the_database_not_at_all() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();

        assert_eq!(seed_unready_deps(&db, &[]).await.unwrap(), 0);
        assert!(became_fetchable(&db, &[]).await.unwrap().is_empty());
        assert!(lost_fetchability(&db, &[]).await.unwrap().is_empty());
        assert!(promote(&db, &[]).await.unwrap().is_empty());
        assert!(unpromote_drv_owners(&db, &[]).await.unwrap().is_empty());
        assert!(statements(db.into_transaction_log()).is_empty());
    }

    /// Becoming fetchable decrements every direct dependent once per edge and
    /// promotes only the dependents that reached zero and pass the gates.
    #[tokio::test]
    async fn became_fetchable_promotes_only_the_dependents_that_reached_zero() {
        let x = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv(x)]])
            .append_query_results([vec![ripple_row(d1, true), ripple_row(d2, false)]])
            .append_query_results([vec![drv(d1)]])
            .into_connection();

        let changes = became_fetchable(&db, &[x]).await.unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].derivation, d1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Created, BuildStatus::Queued)
        );

        let log = statements(db.into_transaction_log());
        assert_eq!(log.len(), 3, "mark, ripple, promote: {log:?}");
        assert!(
            log[0].contains("SET fetchable = true") && log[0].contains("NOT db.fetchable"),
            "{log:?}"
        );
        assert!(log[1].contains("unready_deps - c.n"), "{log:?}");
        assert!(
            log[2].contains(&d1.to_string()) && !log[2].contains(&d2.to_string()),
            "only the dependent that reached zero is a candidate: {log:?}"
        );
    }

    /// A flip that changed nothing ripples nothing: the mark returns no row, and no
    /// further statement runs. Rippling a state instead of a transition is what
    /// drives a dependent's counter below zero, where no gate reads it again.
    #[tokio::test]
    async fn an_anchor_already_fetchable_moves_nobody() {
        let x = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let changes = became_fetchable(&db, &[x]).await.unwrap();

        assert!(changes.is_empty());
        assert_eq!(statements(db.into_transaction_log()).len(), 1);
    }

    /// Losing fetchability increments every direct dependent and pulls the queued
    /// ones back to Created; a Created or Building dependent only counts up.
    #[tokio::test]
    async fn lost_fetchability_unpromotes_queued_dependents() {
        let x = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv(x)]])
            .append_query_results([vec![transition_row(d1, 1, 0), transition_row(d2, 0, 0)]])
            .into_connection();

        let changes = lost_fetchability(&db, &[x]).await.unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].derivation, d1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Queued, BuildStatus::Created)
        );

        let log = statements(db.into_transaction_log());
        assert_eq!(log.len(), 2, "mark, ripple: {log:?}");
        assert!(
            log[0].contains("SET fetchable = false") && log[0].contains("db.fetchable AND NOT"),
            "{log:?}"
        );
        assert!(
            log[1].contains("unready_deps + c.n")
                && log[1].contains("CASE WHEN d.status = 1 THEN 0"),
            "{log:?}"
        );
    }

    /// Promotion embeds the whole gate and moves Created rows only, so a candidate
    /// list bounds the statement without asserting anything about the rows in it.
    #[test]
    fn promote_embeds_the_promotable_predicate() {
        for sql in [norm(&PROMOTE), norm(&PROMOTE_ANY)] {
            assert!(sql.contains("SET status = 1"), "{sql}");
            assert!(sql.contains(&norm(&promotable_predicate("db"))), "{sql}");
            assert!(sql.contains("RETURNING db.derivation"), "{sql}");
        }

        assert!(norm(&PROMOTE).contains("db.derivation = ANY($1::uuid[])"));

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

    /// A `.drv` owner leaves the queue only while the `.drv` really is unimportable,
    /// so a re-push between the loss and this call keeps its anchor queued.
    #[test]
    fn unpromoting_a_drv_owner_rechecks_the_drv_itself() {
        let sql = norm(&UNPROMOTE_DRV_OWNERS);
        assert!(sql.contains("d.hash = ANY($1::text[])"), "{sql}");
        assert!(
            sql.contains(&format!(
                "AND NOT (db.substitutable OR {})",
                norm(&drv_whole_predicate("db"))
            )),
            "{sql}"
        );
        assert!(sql.contains("db.status = 1"), "queued rows only: {sql}");
        assert!(
            sql.contains("RETURNING db.derivation, old.status AS from_status"),
            "{sql}"
        );
    }

    /// The repair recomputes over the pending anchors' dependencies first, so the
    /// counter recompute reads repaired inputs, and writes each column as a
    /// compare-and-swap on the pre-image rather than an unconditional overwrite.
    #[test]
    fn repair_statements_cover_dependencies_then_pending() {
        let f = norm(&REPAIR_FETCHABLE);
        let u = norm(&REPAIR_UNREADY);
        assert!(
            f.contains("q.status IN (0, 1)") && f.contains("SELECT e.dependency"),
            "one edge past the pending set: {f}"
        );
        assert!(
            f.contains("db.fetchable = x.old AND x.old <> x.f"),
            "compare-and-swap plus drift filter: {f}"
        );
        assert!(
            u.contains("p.status IN (0, 1)") && u.contains("NOT dep.fetchable"),
            "{u}"
        );
        assert!(
            u.contains("db.unready_deps = x.old AND x.old <> x.n"),
            "compare-and-swap plus drift filter: {u}"
        );
    }

    /// The repair order is load-bearing: fetchable, then the counter that reads it,
    /// then the queue. The un-promote may run before the promote only because the
    /// gates never read `status`.
    #[tokio::test]
    async fn the_repair_fixes_fetchable_before_it_recounts_then_settles_the_queue() {
        let demoted = DerivationId::now_v7();
        let promoted = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(2), exec(3)])
            .append_query_results([vec![transition_row(demoted, 1, 0)]])
            .append_query_results([vec![drv(promoted)]])
            .into_connection();

        let repaired = repair_pending(&db).await.unwrap();

        assert_eq!((repaired.fetchable, repaired.unready_deps), (2, 3));
        assert_eq!(repaired.unpromoted.len(), 1);
        assert_eq!(repaired.unpromoted[0].derivation, demoted);
        assert_eq!(repaired.promoted.len(), 1);
        assert_eq!(repaired.promoted[0].derivation, promoted);

        let log = statements(db.into_transaction_log());
        assert_eq!(log.len(), 4, "{log:?}");
        assert!(log[0].contains("SET fetchable = x.f"), "{log:?}");
        assert!(log[1].contains("SET unready_deps = x.n"), "{log:?}");
        assert!(log[2].contains("SET status = 0"), "{log:?}");
        assert!(log[3].contains("SET status = 1"), "{log:?}");
    }
}
