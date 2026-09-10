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
//! row that did not just flip, or rippling one frontier twice, moves a dependent past
//! zero, and a negative counter never satisfies `= 0` again. [`became_fetchable`] and
//! [`lost_fetchability`] therefore mark first and ripple only from the rows their own
//! `RETURNING` reports. `NOT db.fetchable` (respectively `db.fetchable`) is a column
//! of the row being updated, so Postgres re-checks it under EvalPlanQual after any
//! lock wait, and two concurrent markers cannot both claim one flip.
//!
//! # One level, not a fixpoint
//!
//! The NAR ripple recurses because wholeness is transitive: a path that becomes whole
//! makes its referrer whole. Readiness is not. A dependent that reaches zero becomes
//! QUEUED, not fetchable - only a finished build or an upstream copy makes an anchor
//! fetchable - so the frontier stops at the direct dependents and one statement per
//! flip is the entire ripple. Nothing here can change an anchor's own fetchability
//! either: both ripples write `unready_deps`, and the only status they touch is a
//! move inside `{Created, Queued}`, which can neither enter nor leave the
//! terminal-success pair the predicate reads.
//!
//! # Every write runs under a lock proof
//!
//! A flip is a mark plus a compensating ripple, and they are separate statements. On
//! a pooled handle that is two implicit transactions: a ripple killed by a deadlock or
//! a statement timeout leaves the flip committed with its counter move gone, and
//! retrying the call is a silent NO-OP, because the mark no longer matches the row it
//! already flipped. So the pair is not merely better inside one transaction, it is
//! only correct there, and for [`lost_fetchability`] the lost move is the fail-open
//! direction: the dependents keep `unready_deps` too LOW, stay promotable, and
//! dispatch against an input nothing can provide.
//!
//! [`AnchorLock`] is how that is required rather than requested. [`lock_anchors`]
//! takes the anchors `FOR UPDATE` in one `derivation`-ordered statement and returns
//! the only proof [`seed_unready_deps`], [`became_fetchable`] and
//! [`lost_fetchability`] accept, so none of them can run on a pooled handle, in
//! another transaction, or over a row the lock did not name. A retry is then a retry
//! of the whole flip.
//!
//! The RIPPLES are outside that discipline, deliberately, and this is the one thing
//! the proof does not cover: they write the flipped anchors' DEPENDENTS, which no
//! ordered lock names, in whatever order their plan produces. They move the counter
//! relative to the row's own value, so they compose with a concurrent move and need no
//! lock to be correct; what they can do is deadlock against another ripple or against
//! the repair's ordered pass. Postgres detects that rather than hanging, and because
//! the mark and the ripple now share a transaction the detection rolls the flip back
//! with them, which is what makes the caller's retry mean something.
//!
//! # What the repair covers, exactly
//!
//! [`repair_pending`] materialises its scope and then chunks it: per chunk, one
//! transaction takes [`lock_anchors`] and recounts only the rows that chunk names. The
//! lock is load-bearing, not hygiene. An UNLOCKED absolute recount reads its new value
//! from the statement's snapshot while its compare-and-swap reads the stored one from
//! the fresh row version, because EvalPlanQual re-checks only the target row's own
//! columns and never re-evaluates the predicate's subqueries; a concurrent change that
//! leaves the stored value equal to the snapshot's passes the swap and writes a value
//! that was already false. Measured on Postgres 18: an anchor stored
//! `fetchable = false` whose outputs the recount's snapshot saw whole, racing a retire
//! of the only output, ends stored `true` with a true value of `false`, and
//! `fetchable = true` is exactly what stops it counting toward its dependents'
//! `unready_deps`. Chunking is the other half: one transaction over the whole scope
//! holds `FOR UPDATE` on every pending anchor while it works, and a statement timeout
//! or the sweep's budget then cancels it in place and rolls back every repair,
//! silently.
//!
//! Both `fetchable` recounts finish, across every chunk, before the first counter
//! recount starts. A counter computed from a `fetchable = true` that a later chunk was
//! about to correct is too LOW, and too low promotes.
//!
//! Repaired: both columns, over the pending anchors and their direct dependencies as
//! of the scope select. Written: `unready_deps` for every dependent of a flipped
//! anchor at ANY status and for every anchor a caller seeds, `fetchable` for every
//! anchor a caller flips. The repaired set is therefore NARROWER than the written set,
//! and these populations have no backstop at all:
//!
//! - `unready_deps` on a `Building`, `FailedTransient` or terminal row that no pending
//!   anchor depends on. Both ripples write it and no recount visits it. A requeue thaws
//!   it back to `Created`, where the very next promote pass can read the drifted value:
//!   too high stalls the anchor, too low dispatches it against a missing input.
//! - `fetchable` on an anchor that no pending anchor depends on, outside the scope by
//!   construction.
//!
//! The second was worse than it reads, and is why [`seed_unready_deps`] EVALUATES
//! [`crate::graph_sql::fetchable_predicate`] on the dependencies it counts instead of
//! reading their `fetchable` column. The seed runs inside the ingest transaction the
//! moment a dependent's edges land, so it is the first reader of a dependency's
//! readiness and strictly precedes any sweep; reading a stale flag there seeds a fresh
//! dependent to zero, promotes it and dispatches it, and the repair then heals the flag
//! after the build it caused. Evaluating the predicate costs a correlated subquery per
//! edge, once per ingest batch rather than once per tick, which is where that cost
//! belongs. Every other reader of `unready_deps` is
//! [`crate::graph_sql::gates_predicate`] on a `Created` or `Queued` row, which the
//! repair does cover.
//!
//! `unready_deps` carries no `CHECK (unready_deps >= 0)` and must not get one: a ripple
//! legitimately passes through intermediate values inside its own transaction, so the
//! constraint would abort correct work. `cached_path.missing_references` omits it for
//! the same reason.
//!
//! # What this module deliberately leaves to its callers
//!
//! A flip re-checks exactly one of the gate's four inputs. An anchor's own queue
//! membership is not a function of its own `fetchable`, so neither flip touches it:
//! [`lost_fetchability`] moves the DEPENDENTS of the anchors it flipped and never the
//! anchors themselves. The other three inputs each need their own call. A finished walk
//! setting `walked` is [`promote_closure`]; a `build_job` appearing and a `.drv`
//! becoming whole are [`promote`]; a `.drv` ceasing to be whole is
//! [`unpromote_drv_owners`]. `substitutable` being cleared on a `Queued` anchor is the
//! one with no entry point here, because it both unfetches the anchor and fails the
//! anchor's own gate: the caller that clears it owes that anchor a re-check of its own
//! gate, and until it does, [`repair_pending`]'s un-promote pass settles it one sweep
//! later.
//!
//! `m20260908_000002` carries a frozen copy of
//! [`crate::graph_sql::fetchable_predicate`] and of the gates, and there is
//! deliberately NO test asserting the two are equal. `gradient-db` depends on
//! `gradient-migration`, so such a test would compile; it would also be correct only
//! until the live predicate legitimately evolves, and would then fail for a good reason
//! while pressuring someone into editing a shipped migration, which must never happen.
//! That agreement is verified once, by review, at the commit that introduces both.

use crate::graph_sql::{
    drv_whole_predicate, eval_closure_cte, fetchable_predicate, gates_predicate,
    promotable_predicate,
};
use crate::promotion::{returned_derivations, returned_transitions, transitions_from};
use crate::status::TransitionChange;
use crate::status_sql;
use gradient_entity::build::BuildStatus;
use gradient_types::{DerivationId, EvaluationId};
use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseTransaction, DbErr, Statement, TransactionTrait,
    Value,
};
use std::sync::LazyLock;

/// The value `unready_deps` holds for anchor `{alias}`: its direct dependencies whose
/// anchor row is absent, or present and not ready by `dep_ready`.
///
/// One fragment, so the seed and the recount cannot drift on the part that must not:
/// one count per EDGE, and a `LEFT JOIN` so a dependency with NO anchor row counts as
/// unready instead of being dropped. An inner join there fails OPEN, in a gate whose
/// entire job is to stop a dispatch against a missing input.
///
/// `dep_ready` is the one thing the two callers differ on, and deliberately: the
/// recount reads the `fetchable` column it has just repaired, the seed evaluates the
/// predicate itself. See [`seed_unready_deps`].
///
/// Self-edges are NOT excluded, unlike `nar_closure`'s reference count: a store path
/// routinely references itself, a derivation cannot be its own input, and excluding
/// them here would diverge from the frozen backfill for nothing.
fn unready_dependency_count(alias: &str, dep_ready: &str) -> String {
    format!(
        "(SELECT count(*) FROM derivation_dependency e \
         LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
         WHERE e.derivation = {alias}.derivation \
           AND (dep.derivation IS NULL OR NOT ({dep_ready})))"
    )
}

static SEED_UNREADY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET unready_deps = {count} \
         WHERE db.derivation = ANY($1::uuid[])",
        count = unready_dependency_count("db", &fetchable_predicate("dep")),
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

/// The queue's own backstop: every `Queued` anchor whose gates no longer hold.
///
/// Table-wide with no index-friendly bound - it scans the `Queued` rows and evaluates
/// three `EXISTS` plus the negated gate for each, unlike [`PROMOTE_ANY`], which
/// matches a partial index. It is the sweep's most expensive statement and its row
/// count belongs in whatever the sweep reports.
static UNPROMOTE_UNGATED: LazyLock<String> =
    LazyLock::new(|| unpromote_sql(&format!("NOT {gates}", gates = gates_predicate("db"))));

/// The pending anchors and their direct dependencies: every row whose `fetchable` a
/// gate can read this pass, one edge deep. [`pending_scope`] materialises it, so the
/// lock and the recounts name one frozen list rather than a subquery each statement
/// re-evaluates against its own snapshot.
fn repair_scope() -> String {
    let pending = status_sql::build_in(&BuildStatus::PENDING);
    format!(
        "SELECT q.derivation FROM derivation_build q WHERE q.status IN ({pending}) \
       UNION \
         SELECT e.dependency FROM derivation_dependency e \
         JOIN derivation_build q ON q.derivation = e.derivation \
         WHERE q.status IN ({pending})"
    )
}

static RECOUNT_FETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = x.f \
         FROM (SELECT p.derivation, p.fetchable AS old, {pred} AS f \
               FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])) x \
         WHERE db.derivation = x.derivation AND db.fetchable = x.old AND x.old <> x.f",
        pred = fetchable_predicate("p"),
    )
});

static RECOUNT_UNREADY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET unready_deps = x.n \
         FROM (SELECT p.derivation, p.unready_deps AS old, {count} AS n \
               FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])) x \
         WHERE db.derivation = x.derivation AND db.unready_deps = x.old AND x.old <> x.n",
        count = unready_dependency_count("p", "dep.fetchable"),
    )
});

const LOCK_ANCHORS: &str = "SELECT 1 FROM derivation_build \
                            WHERE derivation = ANY($1::uuid[]) \
                            ORDER BY derivation FOR UPDATE";

/// Proof that a batch of anchors is held `FOR UPDATE`, `derivation`-ordered, on `txn`.
/// Only [`lock_anchors`] constructs one, and [`seed_unready_deps`],
/// [`became_fetchable`], [`lost_fetchability`] and the two recounts accept nothing
/// else, so none of them can run unlocked, on a pooled handle where the lock is
/// released at the end of the statement that took it, in another transaction, or over
/// a row the lock did not name.
///
/// It also makes a flip ATOMIC, which is the property the counter actually needs: the
/// mark and its compensating ripple land or roll back together, so a killed ripple
/// leaves nothing to retry around. What the proof does NOT cover is the ripple's own
/// write set, the flipped anchors' dependents, which no ordered lock names; the module
/// doc says why that is sound and what it costs. The caveat on
/// [`crate::nar_closure::ReferenceLock`] applies here too: the proof says the write
/// follows the lock, not that no read preceded it.
#[must_use = "a lock proves nothing unless a write runs on it"]
pub struct AnchorLock<'txn> {
    txn: &'txn DatabaseTransaction,
    derivations: Vec<DerivationId>,
}

/// Take `derivations` `FOR UPDATE` in one `derivation`-ordered statement, before the
/// caller decides anything. With acquisition monotone in `derivation` a wait-for cycle
/// would need some transaction to wait on a lower id than one it already holds; the
/// ripples acquire in plan order and are outside that, which the module doc accounts
/// for. An empty batch locks nothing and issues no statement.
pub async fn lock_anchors<'txn>(
    txn: &'txn DatabaseTransaction,
    derivations: &[DerivationId],
) -> Result<AnchorLock<'txn>, DbErr> {
    if !derivations.is_empty() {
        txn.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            LOCK_ANCHORS,
            [ids(derivations)],
        ))
        .await?;
    }

    Ok(AnchorLock {
        txn,
        derivations: derivations.to_vec(),
    })
}

fn ids(derivations: &[DerivationId]) -> Value {
    derivations
        .iter()
        .map(|d| d.into_inner())
        .collect::<Vec<uuid::Uuid>>()
        .into()
}

/// Recount, for each locked anchor, the direct dependencies that cannot yet serve
/// their outputs, and write it absolutely. Returns the rows written.
///
/// Run it for a derivation whose edges just landed, in the transaction that wrote
/// them, which is what the lock proof requires: the count is over
/// `derivation_dependency`, so an edge inserted after the seed is one a later ripple
/// can cancel without it ever having been counted. It overwrites rather than adjusts
/// on purpose - the value it replaces may be the migration's backfill, which
/// inner-joined the dependency anchor and so counted a dependency with no anchor row
/// as ready.
///
/// It evaluates [`crate::graph_sql::fetchable_predicate`] on each dependency rather
/// than reading the `fetchable` column, because this is the first reader of a
/// dependency's readiness and it runs strictly before any sweep could have corrected
/// a stale flag. The module doc has the full argument and the cost.
pub async fn seed_unready_deps(lock: &AnchorLock<'_>) -> Result<u64, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(0);
    }

    Ok(lock
        .txn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            SEED_UNREADY.as_str(),
            [ids(&lock.derivations)],
        ))
        .await?
        .rows_affected())
}

async fn mark(lock: &AnchorLock<'_>, to: bool) -> Result<Vec<DerivationId>, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(Vec::new());
    }

    let statement = if to {
        MARK_FETCHABLE.as_str()
    } else {
        MARK_UNFETCHABLE.as_str()
    };

    Ok(returned_derivations(
        lock.txn
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                statement,
                [ids(&lock.derivations)],
            ))
            .await?,
    ))
}

/// Flip the locked anchors to fetchable where the predicate now holds, decrement their
/// direct dependents' counters, and queue the dependents that reached zero.
///
/// The returned transitions are `Created` to `Queued`, and the ripple runs only over
/// the rows the mark actually flipped: a caller that hands over an anchor that was
/// already fetchable gets no statement past the mark, which is what keeps a dependent's
/// counter from going below zero.
pub async fn became_fetchable(lock: &AnchorLock<'_>) -> Result<Vec<TransitionChange>, DbErr> {
    let flipped = mark(lock, true).await?;
    if flipped.is_empty() {
        return Ok(Vec::new());
    }

    let rows = lock
        .txn
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

    promote(lock.txn, &ready).await
}

/// Flip the locked anchors to not fetchable where the predicate no longer holds,
/// increment their direct dependents' counters, and pull the queued dependents back to
/// `Created`. The returned transitions are `Queued` to `Created`; a dependent that was
/// `Created`, `Building` or terminal only counts up.
pub async fn lost_fetchability(lock: &AnchorLock<'_>) -> Result<Vec<TransitionChange>, DbErr> {
    let flipped = mark(lock, false).await?;
    if flipped.is_empty() {
        return Ok(Vec::new());
    }

    let rows = lock
        .txn
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

/// A `.drv` that is no longer whole cannot be imported, so its owner leaves the queue
/// unless an upstream serves it. The statement re-checks that ground truth rather than
/// trusting the hash list: a `.drv` re-pushed between the loss and this call must keep
/// its anchor queued.
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

/// Materialise [`repair_scope`] once, so every chunk the repair locks and recounts
/// comes from one snapshot instead of a subquery each statement re-evaluates against
/// its own.
async fn pending_scope<C: ConnectionTrait>(db: &C) -> Result<Vec<DerivationId>, DbErr> {
    db.query_all_raw(Statement::from_string(
        DatabaseBackend::Postgres,
        repair_scope(),
    ))
    .await?
    .into_iter()
    .map(|r| {
        r.try_get::<uuid::Uuid>("", "derivation")
            .map(DerivationId::new)
    })
    .collect()
}

/// Recompute `fetchable` for the locked chunk and write the rows that disagree.
async fn recount_fetchable(lock: &AnchorLock<'_>) -> Result<u64, DbErr> {
    recount(lock, RECOUNT_FETCHABLE.as_str()).await
}

/// Recompute `unready_deps` for the locked chunk and write the rows that disagree.
async fn recount_unready(lock: &AnchorLock<'_>) -> Result<u64, DbErr> {
    recount(lock, RECOUNT_UNREADY.as_str()).await
}

async fn recount(lock: &AnchorLock<'_>, statement: &str) -> Result<u64, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(0);
    }

    Ok(lock
        .txn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            statement,
            [ids(&lock.derivations)],
        ))
        .await?
        .rows_affected())
}

/// Recompute both columns over the pending anchors and their direct dependencies,
/// write only what differs, and settle the queue against the gates.
///
/// One transaction per chunk, each taking [`lock_anchors`] before it recounts, so no
/// recount writes a row it did not lock and a cancelled sweep loses one chunk rather
/// than every repair. `fetchable` is recounted over EVERY chunk before the first
/// counter recount runs, because a counter computed from a stale `fetchable = true` is
/// too low and promotes. The module doc has the measured failure behind both.
///
/// The compare-and-swap on the pre-image (`db.fetchable = x.old AND x.old <> x.f`)
/// stays. Under the lock it compares the row to itself, but a future caller that loses
/// the lock degrades to a skipped row rather than to an unconditional overwrite of a
/// value nothing else re-derives, and `old <> new` is the drift filter behind the
/// returned counts.
///
/// The queue settles after the chunks, on the caller's handle. Both statements re-check
/// the target row's own `status` and `unready_deps`, which EvalPlanQual does
/// re-evaluate, so they are exactly as safe as the live promotion path and no safer:
/// the gate's three `EXISTS` subqueries are not re-evaluated, so a promote can still
/// fire on a `.drv` retired during a lock wait. The un-promote runs first, and the two
/// cannot both move a row because [`crate::graph_sql::gates_predicate`] never reads
/// `status`.
pub async fn repair_pending<C>(db: &C) -> Result<Repaired, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let scope = pending_scope(db).await?;
    let mut fetchable = 0u64;
    for chunk in scope.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        let lock = lock_anchors(&txn, chunk).await?;
        fetchable += recount_fetchable(&lock).await?;
        txn.commit().await?;
    }

    let mut unready_deps = 0u64;
    for chunk in scope.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        let lock = lock_anchors(&txn, chunk).await?;
        unready_deps += recount_unready(&lock).await?;
        txn.commit().await?;
    }

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

    /// A dependency with no anchor row at all must count as UNREADY. An inner join
    /// counted zero and failed open, in a gate whose whole job is to stop a dispatch
    /// against a missing input.
    #[test]
    fn the_count_treats_a_dependency_with_no_anchor_as_unready() {
        let sql = norm(&unready_dependency_count("db", "dep.fetchable"));
        assert_eq!(
            sql,
            "(SELECT count(*) FROM derivation_dependency e \
             LEFT JOIN derivation_build dep ON dep.derivation = e.dependency \
             WHERE e.derivation = db.derivation \
             AND (dep.derivation IS NULL OR NOT (dep.fetchable)))"
        );
    }

    /// The seed writes the count absolutely, so it never accumulates onto - or trusts
    /// - the value it finds, and a leaf gets zero from an empty count.
    #[test]
    fn the_seed_overwrites_and_never_adjusts() {
        let sql = norm(&SEED_UNREADY);
        assert!(
            sql.starts_with("UPDATE derivation_build db SET unready_deps = (SELECT count(*)"),
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

    /// The seed is the first reader of a dependency's readiness and runs before any
    /// sweep could have corrected a stale flag, so it must EVALUATE the predicate on
    /// each dependency rather than read the column. The recount, which runs right
    /// after its own repair of that column, reads the column.
    #[test]
    fn the_seed_evaluates_the_predicate_while_the_recount_reads_the_column() {
        let seed = norm(&SEED_UNREADY);
        assert!(
            seed.contains(&norm(&fetchable_predicate("dep"))),
            "the seed must evaluate the predicate on its dependencies: {seed}"
        );
        assert!(
            !seed.contains("NOT (dep.fetchable)"),
            "the seed must not trust the column: {seed}"
        );

        let recount = norm(&RECOUNT_UNREADY);
        assert!(recount.contains("NOT (dep.fetchable)"), "{recount}");
        assert!(
            !recount.contains(&norm(&fetchable_predicate("dep"))),
            "the recount reads the column it just repaired: {recount}"
        );
    }

    /// An empty batch is not a statement, the lock included: every entry point
    /// short-circuits so a caller can hand over whatever its event produced.
    #[tokio::test]
    async fn an_empty_batch_touches_the_database_not_at_all() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let txn = db.begin().await.unwrap();
        let lock = lock_anchors(&txn, &[]).await.unwrap();

        assert_eq!(seed_unready_deps(&lock).await.unwrap(), 0);
        assert!(became_fetchable(&lock).await.unwrap().is_empty());
        assert!(lost_fetchability(&lock).await.unwrap().is_empty());
        txn.commit().await.unwrap();

        assert!(promote(&db, &[]).await.unwrap().is_empty());
        assert!(unpromote_drv_owners(&db, &[]).await.unwrap().is_empty());
        assert!(statements(db.into_transaction_log()).is_empty());
    }

    /// The mark and its compensating ripple must land or roll back together: on a
    /// pooled handle a killed ripple leaves the flip committed and the counter move
    /// gone, and the retry marks nothing so it ripples nothing. The lock proof is what
    /// makes that impossible to write, so it must be the only way in.
    #[tokio::test]
    async fn a_flip_and_its_ripple_share_the_locking_transaction() {
        let x = DerivationId::now_v7();
        let d = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1)])
            .append_query_results([vec![drv(x)]])
            .append_query_results([vec![ripple_row(d, false)]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_anchors(&txn, &[x]).await.unwrap();
        became_fetchable(&lock).await.unwrap();
        txn.commit().await.unwrap();

        let raw = db.into_transaction_log();
        assert_eq!(raw.len(), 1, "one transaction for the whole flip: {raw:?}");

        let inside: Vec<&str> = raw[0].statements().iter().map(|s| s.sql.as_str()).collect();
        assert_eq!(
            inside.len(),
            5,
            "BEGIN, lock, mark, ripple, COMMIT: {inside:?}"
        );
        assert!(
            inside[1].contains("ORDER BY derivation FOR UPDATE"),
            "an unordered locker deadlocks against every ordered one: {inside:?}"
        );
        assert!(inside[2].contains("SET fetchable = true"), "{inside:?}");
        assert!(inside[3].contains("unready_deps - c.n"), "{inside:?}");
    }

    /// Becoming fetchable decrements every direct dependent once per edge and promotes
    /// only the dependents that reached zero and pass the gates. Two anchors go in and
    /// one flips, so an implementation that rippled the caller's list instead of the
    /// mark's `RETURNING` writes the wrong frontier and fails here - which is the
    /// regression the module doc calls unrecoverable.
    #[tokio::test]
    async fn became_fetchable_ripples_the_marks_returning_and_promotes_only_zeroes() {
        let flipped = DerivationId::now_v7();
        let untouched = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(2)])
            .append_query_results([vec![drv(flipped)]])
            .append_query_results([vec![ripple_row(d1, true), ripple_row(d2, false)]])
            .append_query_results([vec![drv(d1)]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_anchors(&txn, &[flipped, untouched]).await.unwrap();
        let changes = became_fetchable(&lock).await.unwrap();
        txn.commit().await.unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].derivation, d1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Created, BuildStatus::Queued)
        );

        let log = statements(db.into_transaction_log());
        assert_eq!(log.len(), 4, "lock, mark, ripple, promote: {log:?}");
        assert!(
            log[1].contains(&flipped.to_string()) && log[1].contains(&untouched.to_string()),
            "the mark is offered both anchors: {log:?}"
        );
        assert!(
            log[2].contains(&flipped.to_string()) && !log[2].contains(&untouched.to_string()),
            "the ripple takes only the anchor the mark returned: {log:?}"
        );
        assert!(
            log[3].contains(&d1.to_string()) && !log[3].contains(&d2.to_string()),
            "only the dependent that reached zero is a candidate: {log:?}"
        );
    }

    /// A flip that changed nothing ripples nothing: the mark returns no row, and no
    /// further statement runs. Rippling a state instead of a transition is what drives
    /// a dependent's counter below zero, where no gate reads it again.
    #[tokio::test]
    async fn an_anchor_already_fetchable_moves_nobody() {
        let x = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1)])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_anchors(&txn, &[x]).await.unwrap();
        let changes = became_fetchable(&lock).await.unwrap();
        txn.commit().await.unwrap();

        assert!(changes.is_empty());
        assert_eq!(statements(db.into_transaction_log()).len(), 2, "lock, mark");
    }

    /// Losing fetchability increments every direct dependent and pulls the queued ones
    /// back to Created; a Created or Building dependent only counts up.
    #[tokio::test]
    async fn lost_fetchability_unpromotes_queued_dependents() {
        let x = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1)])
            .append_query_results([vec![drv(x)]])
            .append_query_results([vec![transition_row(d1, 1, 0), transition_row(d2, 0, 0)]])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_anchors(&txn, &[x]).await.unwrap();
        let changes = lost_fetchability(&lock).await.unwrap();
        txn.commit().await.unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].derivation, d1);
        assert_eq!(
            (changes[0].from, changes[0].to),
            (BuildStatus::Queued, BuildStatus::Created)
        );

        let log = statements(db.into_transaction_log());
        assert_eq!(log.len(), 3, "lock, mark, ripple: {log:?}");
        assert!(
            log[1].contains("SET fetchable = false") && log[1].contains("db.fetchable AND NOT"),
            "{log:?}"
        );
        assert!(
            log[2].contains("unready_deps + c.n")
                && log[2].contains("CASE WHEN d.status = 1 THEN 0"),
            "{log:?}"
        );
    }

    /// Promotion embeds the whole gate and moves Created rows only, so a candidate list
    /// bounds the statement without asserting anything about the rows in it.
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

    /// A `.drv` owner leaves the queue only while the `.drv` really is unimportable, so
    /// a re-push between the loss and this call keeps its anchor queued.
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

    /// Both recounts are bounded by the chunk their lock named, never by a subquery a
    /// second statement would re-evaluate against its own snapshot, and both write as a
    /// compare-and-swap on the pre-image rather than an unconditional overwrite.
    #[test]
    fn the_recounts_are_compare_and_swaps_over_the_locked_chunk() {
        for sql in [norm(&RECOUNT_FETCHABLE), norm(&RECOUNT_UNREADY)] {
            assert!(
                sql.contains("FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])"),
                "the chunk is the whole bound: {sql}"
            );
            assert!(
                !sql.contains("WHERE p.status IN"),
                "a status scope would widen a recount past its lock: {sql}"
            );
        }

        assert!(
            norm(&RECOUNT_FETCHABLE).contains("db.fetchable = x.old AND x.old <> x.f"),
            "compare-and-swap plus drift filter"
        );
        assert!(
            norm(&RECOUNT_UNREADY).contains("db.unready_deps = x.old AND x.old <> x.n"),
            "compare-and-swap plus drift filter"
        );
    }

    /// The scope is the pending anchors and one edge past them, and it is SELECTed once
    /// rather than left as a subquery: an inline scope is re-evaluated per statement, so
    /// a row entering it after the lock would be written unlocked, which is the very ABA
    /// the lock exists to stop.
    #[test]
    fn the_scope_reaches_one_edge_past_the_pending_anchors() {
        let sql = norm(&repair_scope());
        assert_eq!(sql.matches("q.status IN (0, 1)").count(), 2, "{sql}");
        assert!(
            sql.contains("SELECT e.dependency FROM derivation_dependency e"),
            "{sql}"
        );
        assert!(
            sql.starts_with("SELECT q.derivation"),
            "the UNION names the first arm's column: {sql}"
        );
    }

    /// Every recount must be preceded by an ordered lock in its own transaction, so no
    /// row is written from a snapshot a concurrent flip could have moved under it.
    #[tokio::test]
    async fn the_repair_locks_each_chunk_before_it_recounts_it() {
        let a = DerivationId::now_v7();
        let demoted = DerivationId::now_v7();
        let promoted = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv(a)]])
            .append_exec_results([exec(0), exec(2), exec(0), exec(3)])
            .append_query_results([vec![transition_row(demoted, 1, 0)]])
            .append_query_results([vec![drv(promoted)]])
            .into_connection();

        let repaired = repair_pending(&db).await.unwrap();

        assert_eq!((repaired.fetchable, repaired.unready_deps), (2, 3));
        assert_eq!(repaired.unpromoted.len(), 1);
        assert_eq!(repaired.unpromoted[0].derivation, demoted);
        assert_eq!(repaired.promoted.len(), 1);
        assert_eq!(repaired.promoted[0].derivation, promoted);

        let raw = db.into_transaction_log();
        assert_eq!(
            raw.len(),
            5,
            "the scope select, one transaction per pass, then the queue: {raw:?}"
        );

        for (entry, recount) in [(1, "SET fetchable = x.f"), (2, "SET unready_deps = x.n")] {
            let inside: Vec<&str> = raw[entry]
                .statements()
                .iter()
                .map(|s| s.sql.as_str())
                .collect();
            assert_eq!(inside.len(), 4, "BEGIN, lock, recount, COMMIT: {inside:?}");
            assert!(
                inside[1].contains("ORDER BY derivation FOR UPDATE"),
                "{inside:?}"
            );
            assert!(inside[2].contains(recount), "{inside:?}");
        }

        let log = statements(raw);
        assert_eq!(log.len(), 7, "{log:?}");
        assert!(log[5].contains("SET status = 0"), "{log:?}");
        assert!(log[6].contains("SET status = 1"), "{log:?}");
    }

    /// Every chunk's `fetchable` recount lands before the first counter recount: a
    /// counter computed from a `fetchable = true` a later chunk was about to correct
    /// comes out too LOW, and too low promotes and dispatches.
    #[tokio::test]
    async fn the_repair_finishes_fetchable_everywhere_before_it_recounts_a_counter() {
        let scope: Vec<BTreeMap<String, Value>> = (0..crate::IN_CHUNK_SIZE + 1)
            .map(|_| drv(DerivationId::now_v7()))
            .collect();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([scope])
            .append_exec_results(vec![exec(1); 8])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let repaired = repair_pending(&db).await.unwrap();

        assert_eq!(
            (repaired.fetchable, repaired.unready_deps),
            (2, 2),
            "one row per chunk per pass"
        );

        let log = statements(db.into_transaction_log());
        let columns: Vec<&str> = log
            .iter()
            .filter_map(|s| {
                if s.contains("SET fetchable = x.f") {
                    Some("fetchable")
                } else if s.contains("SET unready_deps = x.n") {
                    Some("unready_deps")
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(
            columns,
            ["fetchable", "fetchable", "unready_deps", "unready_deps"],
            "two chunks, both fetchable passes first: {log:?}"
        );
    }
}
