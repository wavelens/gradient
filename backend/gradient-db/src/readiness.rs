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
//! Like `derivation_build.missing_runtime_deps`, this counter is MOVED and not derived, so
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
//! [`readiness_scope`] materialises the scope once and the two repairs chunk it: per chunk, one
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
//! Each chunk locks what it WRITES, not what it READS. The counter recount reads
//! `dep.fetchable` for dependencies the chunk does not name, so a flip that commits
//! after the recount's snapshot is invisible to it while the compare-and-swap, which
//! only guards the target row, still passes: the stale count lands, and if the
//! dependency LOST fetchability the count is too low, which promotes. The flip's own
//! ripple writes the same dependent, so whichever of the two commits second wins and
//! the next sweep converges. Closing it would need the lock to cover the transitive
//! read set, which is the unchunked pass this shape exists to avoid.
//! the wholeness recount carries the identical residual for the same
//! reason.
//!
//! Repaired: both columns, over the pending anchors and their direct dependencies as
//! of the scope select, plus `fetchable` wherever the stored flag contradicts a column
//! the row carries: `true` with a runtime hole counted, or terminal success without
//! the flag. The wholeness recount is table-wide and flips nothing, so without those
//! two a counter it raises two hops below anything pending left the flag `true` for
//! good, and a stale `true` is a settled anchor to every walk: 19 such rows stood
//! between 47 builders and the paths retention had taken. Written: `unready_deps` for
//! every dependent of a flipped anchor at ANY status and for every anchor a caller
//! seeds. What still has no backstop is `unready_deps` on a `Building`,
//! `FailedTransient` or terminal row that no pending anchor depends on: both ripples
//! write it and no recount visits it, and a requeue thaws it back to `Created`, where
//! the very next promote pass can read the drifted value.
//!
//! A stale flag was worse than it reads, and is why [`seed_unready_deps`] EVALUATES
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
//! constraint would abort correct work. `missing_runtime_deps` omits it for
//! the same reason.
//!
//! # What this module deliberately leaves to its callers
//!
//! A flip re-checks exactly one of the gate's four inputs. An anchor's own queue
//! membership is not a function of its own `fetchable`, so neither flip touches it:
//! [`lost_fetchability`] moves the DEPENDENTS of the anchors it flipped and never the
//! anchors themselves. What it does re-open is the walk BELOW them: an anchor that
//! stopped being fetchable is open, what its outputs reference is wanted again, and
//! nothing else would ask, so it recomputes demand from the flipped rows. The other
//! three inputs each need their own call. A finished walk
//! setting `walked` is [`promote_closure`]; a `build_job` appearing, a `.drv` becoming
//! whole and demand arriving are [`promote`]; a `.drv` ceasing to be whole is
//! [`unpromote_drv_owners`]; demand going away is [`unpromote_ungated`] over what
//! [`recompute_demand`] reports lost. `substitutable` being cleared on a `Queued` anchor is the
//! one with no entry point here, because it both unfetches the anchor and fails the
//! anchor's own gate: the caller that clears it owes that anchor a re-check of its own
//! gate, and until it does, [`repair_readiness`]'s un-promote pass settles it one sweep
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
    builder_predicate, eval_closure_cte, fetchable_predicate, gates_predicate, open_predicate,
    promotable_predicate,
};
use crate::promotion::{returned_derivations, returned_transitions, transitions_from};
use crate::status::TransitionChange;
use crate::status_sql;
use gradient_entity::build::BuildStatus;
use gradient_types::{DerivationId, EvaluationId};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait, Value};
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

crate::sql_lazy! {
    SEED_UNREADY_QUERY = || SEED_UNREADY.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
}

static MARK_FETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = true \
         WHERE db.derivation = ANY($1::uuid[]) AND NOT db.fetchable AND {pred} \
         RETURNING db.derivation",
        pred = fetchable_predicate("db"),
    )
});

crate::sql_lazy! {
    MARK_FETCHABLE_QUERY = || MARK_FETCHABLE.as_str(),
        params = [DerivationIds(64)];
}

static MARK_UNFETCHABLE: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET fetchable = false \
         WHERE db.derivation = ANY($1::uuid[]) AND db.fetchable AND NOT {pred} \
         RETURNING db.derivation",
        pred = fetchable_predicate("db"),
    )
});

crate::sql_lazy! {
    MARK_UNFETCHABLE_QUERY = || MARK_UNFETCHABLE.as_str(),
        params = [DerivationIds(64)];
}

crate::sql! {
    RIPPLE_DOWN = r#"
    UPDATE derivation_build d
    SET unready_deps = d.unready_deps - c.n
    FROM (SELECT e.derivation, count(*) AS n FROM derivation_dependency e
          WHERE e.dependency = ANY($1::uuid[]) GROUP BY e.derivation) c
    WHERE d.derivation = c.derivation
    RETURNING d.derivation, d.unready_deps = 0 AS ready
"#,
        params = [DerivationIds(64)],
        tier = Bulk;
}

static RIPPLE_UP: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build d \
         SET unready_deps = d.unready_deps + c.n, \
             status = CASE WHEN d.status = {queued} THEN {created} ELSE d.status END, \
             updated_at = CASE WHEN d.status = {queued} \
                               THEN (now() AT TIME ZONE 'UTC') ELSE d.updated_at END \
         FROM derivation_build old, \
              (SELECT e.derivation, count(*) AS n FROM derivation_dependency e \
               WHERE e.dependency = ANY($1::uuid[]) GROUP BY e.derivation) c \
         WHERE d.derivation = c.derivation AND old.id = d.id \
         RETURNING d.derivation, old.status AS from_status, d.status AS to_status",
        queued = status_sql::build(BuildStatus::Queued),
        created = status_sql::build(BuildStatus::Created),
    )
});

crate::sql_lazy! {
    RIPPLE_UP_QUERY = || RIPPLE_UP.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
}

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

crate::sql_lazy! {
    PROMOTE_QUERY = || PROMOTE.as_str(),
        params = [DerivationIds(64)];
}

/// `Created` to `Queued` table-wide. The leading conjunct is implied by the gate (a
/// relay takes the `substitutable` arm, a build the `unready_deps = 0` one) and is
/// written out anyway: it is the predicate of `idx-derivation_build-promotable`, and
/// spelling it makes the implication syntactic.
///
/// It is still the sweep's, not the queue's: the selective term is the gate's
/// `build_job` EXISTS, so the planner rightly drives from the jobs and reaches the
/// anchors through that index rather than scanning it. [`repair_readiness`] is the
/// only caller and it runs table-wide by design.
static PROMOTE_ANY: LazyLock<String> =
    LazyLock::new(|| promote_sql("(db.unready_deps = 0 OR db.substitutable) AND "));

crate::sql_lazy! {
    PROMOTE_ANY_QUERY = || PROMOTE_ANY.as_str(),
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
         FROM derivation_build old \
         WHERE old.id = db.id AND db.status = {queued} AND {reason} \
         RETURNING db.derivation, old.status AS from_status, db.status AS to_status",
        created = status_sql::build(BuildStatus::Created),
        queued = status_sql::build(BuildStatus::Queued),
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

/// The queue's own backstop: every `Queued` anchor whose gates no longer hold.
///
/// Table-wide with no index-friendly bound - it scans the `Queued` rows and evaluates
/// three `EXISTS` plus the negated gate for each, unlike [`PROMOTE_ANY`], which
/// matches a partial index. It is the sweep's most expensive statement and its row
/// count belongs in whatever the sweep reports.
static UNPROMOTE_UNGATED: LazyLock<String> = LazyLock::new(|| unpromote_ungated_sql(""));

crate::sql_lazy! {
    UNPROMOTE_UNGATED_QUERY = || UNPROMOTE_UNGATED.as_str(),
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

/// The pending anchors and their direct dependencies, every row whose `fetchable` a
/// gate can read this pass, and every row whose stored flag contradicts a column it
/// carries, which no pending anchor need be near. [`readiness_scope`] materialises
/// it, so the lock and the recounts name one frozen list rather than a subquery each
/// statement re-evaluates against its own snapshot. The two contradiction arms are
/// partial-index scans: `idx-derivation_build-fetchable-unwhole` and
/// `idx-derivation_build-open`.
fn repair_scope() -> String {
    let pending = status_sql::build_in(&BuildStatus::PENDING);
    let terminal_success = status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS);
    format!(
        "SELECT q.derivation FROM derivation_build q WHERE q.status IN ({pending}) \
       UNION \
         SELECT e.dependency FROM derivation_dependency e \
         JOIN derivation_build q ON q.derivation = e.derivation \
         WHERE q.status IN ({pending}) \
       UNION \
         SELECT q.derivation FROM derivation_build q \
         WHERE q.fetchable AND q.missing_runtime_deps > 0 \
       UNION \
         SELECT q.derivation FROM derivation_build q \
         WHERE q.status IN ({terminal_success}) AND NOT q.fetchable"
    )
}

crate::sql_fn! {
    REPAIR_SCOPE_QUERY = repair_scope,
        params = [],
        tier = Sweep;
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

crate::sql_lazy! {
    RECOUNT_FETCHABLE_QUERY = || RECOUNT_FETCHABLE.as_str(),
        params = [DerivationIds(64)];
}

static RECOUNT_UNREADY: LazyLock<String> = LazyLock::new(|| {
    format!(
        "UPDATE derivation_build db SET unready_deps = x.n \
         FROM (SELECT p.derivation, p.unready_deps AS old, {count} AS n \
               FROM derivation_build p WHERE p.derivation = ANY($1::uuid[])) x \
         WHERE db.derivation = x.derivation AND db.unready_deps = x.old AND x.old <> x.n",
        count = unready_dependency_count("p", "dep.fetchable"),
    )
});

crate::sql_lazy! {
    RECOUNT_UNREADY_QUERY = || RECOUNT_UNREADY.as_str(),
        params = [DerivationIds(64)],
        tier = Bulk;
}

crate::sql! {
    LOCK_ANCHORS = "SELECT 1 FROM derivation_build \
                    WHERE derivation = ANY($1::uuid[]) \
                    ORDER BY derivation FOR UPDATE",
        params = [DerivationIds(64)];
}

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
/// The caveat on every lock proof applies here too: it says the write
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
        txn.execute_raw(LOCK_ANCHORS.bind([ids(derivations)]))
            .await?;
    }

    Ok(AnchorLock {
        txn,
        derivations: derivations.to_vec(),
    })
}

pub(crate) fn ids(derivations: &[DerivationId]) -> Value {
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
/// on purpose: the value it replaces was counted over an older edge set, or is the
/// column default on a row a retire has just reset, and an adjustment would carry
/// that error forward instead of ending it.
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
        .execute_raw(SEED_UNREADY_QUERY.bind([ids(&lock.derivations)]))
        .await?
        .rows_affected())
}

async fn mark(lock: &AnchorLock<'_>, to: bool) -> Result<Vec<DerivationId>, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(Vec::new());
    }

    let query = if to {
        &MARK_FETCHABLE_QUERY
    } else {
        &MARK_UNFETCHABLE_QUERY
    };

    Ok(returned_derivations(
        lock.txn
            .query_all_raw(query.bind([ids(&lock.derivations)]))
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
        .query_all_raw(RIPPLE_DOWN.bind([ids(&flipped)]))
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

/// [`became_fetchable`] with the transaction and the lock it needs, for an event
/// that names its anchors and does nothing else to them.
///
/// `begin` is a real transaction on a pooled handle and a SAVEPOINT on one that
/// already stands for a transaction, and a savepoint's locks are held to the OUTER
/// commit, so this one shape is correct inside the graph actor and outside it. The
/// caller emits the returned transitions after this returns and never between the
/// lock and the commit, which is why they are returned rather than emitted here.
pub async fn advance_fetchable<C>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let txn = db.begin().await?;
    let lock = lock_anchors(&txn, derivations).await?;
    let changes = became_fetchable(&lock).await?;
    txn.commit().await?;

    Ok(changes)
}

/// Flip the locked anchors to not fetchable where the predicate no longer holds,
/// increment their direct dependents' counters, and pull the queued dependents back to
/// `Created`; a dependent that was `Created`, `Building` or terminal only counts up.
///
/// An anchor that stops being fetchable is open again, so the walk below it is
/// re-opened here too: demand is recomputed from the flipped anchors and the queue
/// settled against it, in the flip's own transaction. A `Completed` anchor whose
/// closure just lost a path is the only way the hole below it is reached, and a
/// retire that dropped the flag and asked for nothing left 47 builders waiting
/// behind 22 such anchors.
pub async fn lost_fetchability(lock: &AnchorLock<'_>) -> Result<Vec<TransitionChange>, DbErr> {
    let flipped = mark(lock, false).await?;
    if flipped.is_empty() {
        return Ok(Vec::new());
    }

    let rows = lock
        .txn
        .query_all_raw(RIPPLE_UP_QUERY.bind([ids(&flipped)]))
        .await?;
    let mut changes: Vec<TransitionChange> = returned_transitions(rows)
        .into_iter()
        .filter(|c| c.from != c.to)
        .collect();

    let moved = recompute_demand(lock.txn, &flipped).await?;
    changes.extend(settle_demand(lock.txn, &moved).await?);

    Ok(changes)
}

crate::sql! {
    /// A `Created` anchor nothing demands and no entry point names: a build-time
    /// dependency of something we relay, which will never be built and is not
    /// waiting for anything either. `Skipped` is what settles it on the board.
    ///
    /// Bounded by the ids the demand move reports, so the sweep's table-wide form
    /// below is the only pass that reads the whole table.
    SKIP_UNDEMANDED = "UPDATE derivation_build db SET status = 10, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.derivation = ANY($1::uuid[]) AND db.status = 0 AND NOT db.demanded \
           AND NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation) \
         RETURNING db.derivation, 0 AS from_status, 10 AS to_status",
        params = [DerivationIds(64)];

    /// The mirror: demand came back, so the anchor is pending work again. It goes
    /// to `Created` and not to `Queued` - the promote that follows reads the gates.
    THAW_SKIPPED = "UPDATE derivation_build db SET status = 0, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.derivation = ANY($1::uuid[]) AND db.status = 10 AND db.demanded \
         RETURNING db.derivation, 10 AS from_status, 0 AS to_status",
        params = [DerivationIds(64)];

    /// [`SKIP_UNDEMANDED`] over the whole table: the sweep's backstop for a lost
    /// move, and the backfill for every anchor that was already settled when the
    /// status existed.
    SKIP_UNDEMANDED_ALL = "UPDATE derivation_build db SET status = 10, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.status = 0 AND NOT db.demanded \
           AND NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation) \
         RETURNING db.derivation, 0 AS from_status, 10 AS to_status",
        params = [],
        tier = Sweep;

    THAW_SKIPPED_ALL = "UPDATE derivation_build db SET status = 0, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.status = 10 AND db.demanded \
         RETURNING db.derivation, 10 AS from_status, 0 AS to_status",
        params = [],
        tier = Sweep;
}

/// Settle every `Created` anchor among `candidates` that nothing demands.
pub async fn skip_undemanded<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    Ok(returned_transitions(
        db.query_all_raw(SKIP_UNDEMANDED.bind([ids(candidates)]))
            .await?,
    ))
}

/// Wake every `Skipped` anchor among `candidates` that something wants again.
pub async fn thaw_skipped<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    Ok(returned_transitions(
        db.query_all_raw(THAW_SKIPPED.bind([ids(candidates)]))
            .await?,
    ))
}

/// The sweep's table-wide pair, run after the demand recount so both read a
/// corrected column. Returns every row either moved.
pub async fn settle_skipped<C: ConnectionTrait>(db: &C) -> Result<Vec<TransitionChange>, DbErr> {
    let mut changes = returned_transitions(db.query_all_raw(THAW_SKIPPED_ALL.stmt()).await?);
    changes.extend(returned_transitions(
        db.query_all_raw(SKIP_UNDEMANDED_ALL.stmt()).await?,
    ));

    Ok(changes)
}

/// Table-wide, seeded from every entry point: the backstop for a lost recompute and
/// the backfill the migration deliberately does not carry.
///
/// Absolute rather than incremental for the reason [`seed_unready_deps`] is: an
/// adjustment cannot express "this anchor keeps its demand through a different
/// parent", and an anchor that keeps it re-demands everything below it, which the
/// walk's downward step computes for free. It returns each row it changed WITH its
/// new value, so one statement serves a gain and a loss and no caller has to know
/// which it caused.
///
/// Every open anchor is rewritten and nothing else. A settled anchor keeps whatever
/// it carried, nothing reads it there, and the event that opens it again recomputes
/// it as a root. A `Completed` anchor whose closure has a hole is open, and its
/// value is what the bounded recompute below it reads to seed the hole. The scope is
/// the whole open table by design, so the scan the planner answers it with is the
/// right plan and the tier says so.
pub(crate) static RECOUNT_DEMANDED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "WITH RECURSIVE {cte} \
         UPDATE derivation_build db \
         SET demanded = (db.derivation IN (SELECT derivation FROM demanded)), \
             updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE {open} \
           AND db.demanded <> (db.derivation IN (SELECT derivation FROM demanded)) \
         RETURNING db.derivation, db.demanded",
        cte = crate::graph_sql::open_closure_cte_body("demanded", &open_entry_points(), ""),
        open = open_predicate("db"),
    )
});

/// The roots of demand: every open anchor an entry point of a retained evaluation
/// names, with its builder bit. A fetchable entry point seeds nothing, since what is
/// below it is served from our cache.
fn open_entry_points() -> String {
    format!(
        "SELECT NULL::uuid, db.derivation, ({builder}) FROM entry_point ep \
         JOIN derivation_build db ON db.derivation = ep.derivation \
         JOIN derivation w ON w.id = db.derivation WHERE {open}",
        builder = builder_predicate("db", "w"),
        open = open_predicate("db"),
    )
}

crate::sql_lazy! {
    RECOUNT_DEMANDED = || RECOUNT_DEMANDED_SQL.as_str(),
        params = [],
        tier = Sweep,
        flags = [Walk];
}

/// Rewrite every anchor whose demand drifted. Returns how many disagreed, which is
/// the sweep's `demand_drift`: a healthy fleet reports zero, and a number that keeps
/// coming back names a mover that is not recomputing what it changed.
pub async fn recount_demanded<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph_sql::begin_walk(db).await?;
    let rows = walk.query_all_raw(RECOUNT_DEMANDED.stmt()).await?;
    walk.commit().await?;

    Ok(rows.len() as u64)
}

/// What a bounded recompute moved: the anchors that gained demand, for [`promote`],
/// and the ones that lost it, for [`unpromote_ungated`].
#[derive(Debug, Default)]
pub struct DemandMoved {
    pub gained: Vec<DerivationId>,
    pub lost: Vec<DerivationId>,
}

static RECOMPUTE_DEMAND_SQL: LazyLock<String> = LazyLock::new(|| {
    // The roots enter the region as builders so the walk steps out of them once even
    // where they have just stopped being one; everything below is stepped through
    // only while it is.
    let region = crate::graph_sql::open_closure_cte_body(
        "region",
        "SELECT NULL::uuid AS evaluation, unnest($1::uuid[]) AS derivation, true AS builder",
        "",
    );
    // The parent lookup is fenced with `OFFSET 0` so it stays correlated to the
    // region member. Unfenced, the planner hoists the whole EXISTS out and answers
    // it standalone - a sequential scan of every anchor filtered on `demanded`,
    // which reads the graph to find the parents of a region of a few dozen. It
    // demands what the walk's own step would: an open, demanded parent outside the
    // region, over a runtime edge from anything and over any edge from a builder.
    let parent = format!(
        "EXISTS (SELECT 1 FROM (SELECT e.derivation AS parent, e.kind FROM derivation_dependency e \
                                WHERE e.dependency = r.derivation OFFSET 0) pe \
                 JOIN derivation_build p ON p.derivation = pe.parent \
                 JOIN derivation pw ON pw.id = p.derivation \
                 WHERE p.demanded AND {open} \
                   AND p.derivation NOT IN (SELECT derivation FROM region) \
                   AND (({builder}) OR pe.kind IN (1, 2)))",
        open = open_predicate("p"),
        builder = builder_predicate("p", "pw"),
    );
    let seed = format!(
        "SELECT NULL::uuid, r.derivation, ({builder}) FROM region r \
         JOIN derivation_build rb ON rb.derivation = r.derivation \
         JOIN derivation w ON w.id = rb.derivation \
         WHERE {open} \
           AND (EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = r.derivation) OR {parent})",
        builder = builder_predicate("rb", "w"),
        open = open_predicate("rb"),
    );
    let demanded = crate::graph_sql::open_closure_cte_body(
        "demanded",
        &seed,
        "e.dependency IN (SELECT derivation FROM region)",
    );

    format!(
        "WITH RECURSIVE {region}, {demanded} \
         SELECT DISTINCT r.derivation, \
                (r.derivation IN (SELECT derivation FROM demanded)) AS demanded \
         FROM region r ORDER BY r.derivation",
    )
});

crate::sql_lazy! {
    RECOMPUTE_DEMAND = || RECOMPUTE_DEMAND_SQL.as_str(),
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

crate::sql! {
    /// Apply what [`RECOMPUTE_DEMAND`] read, as values rather than as a membership
    /// test. `WHERE db.derivation IN (SELECT derivation FROM region)` is a predicate
    /// the planner may answer by reading every anchor and filtering, and it does: a
    /// recursive CTE carries no row estimate worth believing, so a region of a few
    /// dozen loses to a sequential scan of the whole table. A bound array estimates
    /// small, drives a nested loop over the unique index, and takes its row locks in
    /// the derivation order the walk sorted them into.
    WRITE_DEMAND = r#"
UPDATE derivation_build db
SET demanded = x.demanded, updated_at = (now() AT TIME ZONE 'UTC')
FROM unnest($1::uuid[], $2::bool[]) AS x(derivation, demanded)
WHERE db.derivation = x.derivation AND db.demanded <> x.demanded
RETURNING db.derivation, db.demanded
"#,
        params = [DerivationIds(64), Bools(false, 64)];
}

/// Write the region's recomputed demand and return the rows that disagreed, which is
/// what [`recompute_demand`] reports as gained and lost. An empty region writes
/// nothing rather than binding two empty arrays.
async fn write_demand(
    txn: &DatabaseTransaction,
    region: &[QueryResult],
) -> Result<Vec<QueryResult>, DbErr> {
    let mut derivations: Vec<uuid::Uuid> = Vec::with_capacity(region.len());
    let mut demanded: Vec<bool> = Vec::with_capacity(region.len());
    for row in region {
        let (Ok(derivation), Ok(want)) = (
            row.try_get::<uuid::Uuid>("", "derivation"),
            row.try_get::<bool>("", "demanded"),
        ) else {
            continue;
        };

        derivations.push(derivation);
        demanded.push(want);
    }

    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    txn.query_all_raw(WRITE_DEMAND.bind([derivations.into(), demanded.into()]))
        .await
}

/// Recompute demand over `roots` and the pending closure below them, after an event
/// that changed whether they carry it.
///
/// The region includes the roots: a thaw makes an anchor a builder again and its own
/// stored value is as stale as its subtree's. Runs under [`lock_anchors`] on the
/// roots; two recomputes over overlapping regions can still interleave, and the
/// sweep's table-wide recount is the backstop that notices.
///
/// Two statements in one transaction: [`RECOMPUTE_DEMAND`] walks and answers, and
/// [`WRITE_DEMAND`] writes the answer it was handed. Naming the region inside the
/// write instead costs a sequential scan of every anchor, for the reason written on
/// that statement. The split widens the window between reading the graph and writing
/// what it implied to a statement boundary; only the roots are locked either way, so
/// the backstop is the same one, and the write still skips a row that already agrees.
pub async fn recompute_demand<C>(db: &C, roots: &[DerivationId]) -> Result<DemandMoved, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if roots.is_empty() {
        return Ok(DemandMoved::default());
    }

    let txn = crate::graph_sql::begin_walk(db).await?;
    let _lock = lock_anchors(&txn, roots).await?;
    let region = txn
        .query_all_raw(RECOMPUTE_DEMAND.bind([ids(roots)]))
        .await?;
    let rows = write_demand(&txn, &region).await?;
    txn.commit().await?;

    let mut moved = DemandMoved::default();
    for row in &rows {
        let (Ok(derivation), Ok(demanded)) = (
            row.try_get::<uuid::Uuid>("", "derivation"),
            row.try_get::<bool>("", "demanded"),
        ) else {
            continue;
        };

        if demanded {
            moved.gained.push(DerivationId::new(derivation));
        } else {
            moved.lost.push(DerivationId::new(derivation));
        }
    }

    Ok(moved)
}

/// Settle the queue against what a [`recompute_demand`] moved: thaw and queue what
/// gained demand, release and skip what lost it.
///
/// Order is load-bearing on both sides. A thaw has to precede the promote or the
/// gate reads a `Skipped` row and passes it over; the skip has to follow the
/// un-promote or it would try to settle a row still `Queued`. One owner, because a
/// caller that got the order wrong would leave an anchor `Skipped` that something
/// had started wanting again, and nothing else ever looks at it.
pub async fn settle_demand<C: ConnectionTrait>(
    db: &C,
    moved: &DemandMoved,
) -> Result<Vec<TransitionChange>, DbErr> {
    let mut changes = Vec::new();
    for gained in moved.gained.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(thaw_skipped(db, gained).await?);
        changes.extend(promote(db, gained).await?);
    }
    for lost in moved.lost.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(unpromote_ungated(db, lost).await?);
        changes.extend(skip_undemanded(db, lost).await?);
    }

    Ok(changes)
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
        .query_all_raw(PROMOTE_QUERY.bind([ids(candidates)]))
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
pub async fn promote_closure<C>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph_sql::begin_walk(db).await?;
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

/// Pull back every `Queued` anchor whose own `.drv` is among `drv_hashes`, and whose
/// gates the loss of that `.drv` closed. The full gate is embedded rather than the
/// `.drv` term alone, so this is [`unpromote_ungated`] scoped by hash: a `.drv`
/// re-pushed between the loss and this call keeps its anchor queued, and a relay,
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
/// embedded, so the candidate list is a bound and never a claim: an anchor a
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
/// next evaluation walks them again: the un-promoted anchors come back as transitions
/// for the caller to fan out with [`crate::status::emit_transition_effects`].
///
/// The un-walk runs first, because it locks `derivation` rows before [`lock_anchors`]
/// and [`unpromote_ungated`] reach `derivation_build` - the class order ingest
/// (`upsert_walked`, then the anchor locks) and the GC's orphan reclaim both take. The
/// anchor pass that follows acquires the un-promoted rows in `derivation` order instead
/// of the one `unpromote_ungated`'s own UPDATE would pick.
pub async fn unwalk_derivations(
    ctx: &crate::DbContext,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    let txn = ctx.worker_db.begin().await?;
    crate::walk_completeness::unwalk(&txn, derivations).await?;
    let _anchors = lock_anchors(&txn, derivations).await?;
    let changes = unpromote_ungated(&txn, derivations).await?;
    txn.commit().await?;

    Ok(changes)
}

/// What the readiness half of the consistency sweep repaired.
#[derive(Debug, Default)]
pub struct Repaired {
    pub unready_deps: u64,
    pub promoted: Vec<TransitionChange>,
    pub unpromoted: Vec<TransitionChange>,
}

/// Materialise [`repair_scope`] once, so every chunk the two repairs lock and
/// recount comes from one snapshot instead of a subquery each statement re-evaluates
/// against its own. Its length is the sweep's `repair_scope`: a measurement, not a
/// violation, since the select is unbounded and each chunk takes `FOR UPDATE` on rows
/// every live graph writer also locks.
pub async fn readiness_scope<C: ConnectionTrait>(db: &C) -> Result<Vec<DerivationId>, DbErr> {
    db.query_all_raw(REPAIR_SCOPE_QUERY.stmt())
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
    recount(lock, &RECOUNT_FETCHABLE_QUERY).await
}

/// Recompute `unready_deps` for the locked chunk and write the rows that disagree.
async fn recount_unready(lock: &AnchorLock<'_>) -> Result<u64, DbErr> {
    recount(lock, &RECOUNT_UNREADY_QUERY).await
}

async fn recount(lock: &AnchorLock<'_>, query: &crate::sql::Query) -> Result<u64, DbErr> {
    if lock.derivations.is_empty() {
        return Ok(0);
    }

    Ok(lock
        .txn
        .execute_raw(query.bind([ids(&lock.derivations)]))
        .await?
        .rows_affected())
}

/// Recompute `fetchable` over `scope` and write only what differs. One transaction
/// per chunk, each taking [`lock_anchors`] before it recounts, so no recount writes a
/// row it did not lock and a cancelled sweep loses one chunk rather than every repair.
///
/// It runs to the end of the scope before [`repair_readiness`] starts, and before the
/// sweep's demand recount: a counter computed from a stale `fetchable = true` is too
/// low and promotes, and a walk that reads one stops at a settled anchor that is not.
/// The compare-and-swap on the pre-image (`db.fetchable = x.old AND x.old <> x.f`)
/// stays. Under the lock it compares the row to itself, but a future caller that loses
/// the lock degrades to a skipped row rather than to an unconditional overwrite of a
/// value nothing else re-derives, and `old <> new` is the drift filter behind the
/// returned count.
pub async fn repair_fetchable<C>(db: &C, scope: &[DerivationId]) -> Result<u64, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut fetchable = 0u64;
    for chunk in scope.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        let lock = lock_anchors(&txn, chunk).await?;
        fetchable += recount_fetchable(&lock).await?;
        txn.commit().await?;
    }

    Ok(fetchable)
}

/// Recompute `unready_deps` over `scope`, chunked and locked as [`repair_fetchable`]
/// is, then settle the queue against the gates on the caller's handle. Both settling
/// statements re-check the target row's own `status` and `unready_deps`, which
/// EvalPlanQual does re-evaluate, so they are exactly as safe as the live promotion
/// path and no safer: the gate's three `EXISTS` subqueries are not re-evaluated, so a
/// promote can still fire on a `.drv` retired during a lock wait. The un-promote runs
/// first, and the two cannot both move a row because
/// [`crate::graph_sql::gates_predicate`] never reads `status`.
pub async fn repair_readiness<C>(db: &C, scope: &[DerivationId]) -> Result<Repaired, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut unready_deps = 0u64;
    for chunk in scope.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        let lock = lock_anchors(&txn, chunk).await?;
        unready_deps += recount_unready(&lock).await?;
        txn.commit().await?;
    }

    let unpromoted = returned_transitions(db.query_all_raw(UNPROMOTE_UNGATED_QUERY.stmt()).await?);
    let promoted = transitions_from(
        returned_derivations(db.query_all_raw(PROMOTE_ANY_QUERY.stmt()).await?),
        BuildStatus::Created,
        BuildStatus::Queued,
    );

    Ok(Repaired {
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

    fn demand_row(id: DerivationId, demanded: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("demanded".to_owned(), Value::from(demanded)),
        ])
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
        assert!(unpromote_ungated(&db, &[]).await.unwrap().is_empty());
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
    /// back to Created; a Created or Building dependent only counts up. Then the walk
    /// below the flipped anchor is re-opened: the demand recompute runs from exactly
    /// the rows the mark returned, under its own raise, in the flip's transaction.
    #[tokio::test]
    async fn lost_fetchability_unpromotes_queued_dependents_and_reopens_the_walk_below() {
        let x = DerivationId::now_v7();
        let d1 = DerivationId::now_v7();
        let d2 = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(1), exec(0), exec(0)])
            .append_query_results([vec![drv(x)]])
            .append_query_results([vec![transition_row(d1, 1, 0), transition_row(d2, 0, 0)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
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
        assert_eq!(
            log.len(),
            6,
            "lock, mark, ripple, then the raised, locked recompute below the flip: {log:?}"
        );
        assert!(
            log[1].contains("SET fetchable = false") && log[1].contains("db.fetchable AND NOT"),
            "{log:?}"
        );
        assert!(
            log[3].contains("SET LOCAL work_mem")
                && log[4].contains(&x.to_string())
                && log[5].contains("region(evaluation, derivation, builder)"),
            "the recompute is rooted at the anchor the mark flipped: {log:?}"
        );
        assert!(
            log[2].contains("unready_deps + c.n")
                && log[2].contains("CASE WHEN d.status = 1 THEN 0"),
            "{log:?}"
        );
        assert!(
            log[2].contains("updated_at = CASE WHEN d.status = 1"),
            "a dependent that only counted up keeps its updated_at: a FailedTransient \
             row's retry backoff is measured from that column, so bumping it here \
             restarts the window a dependency's regression had nothing to do with: {log:?}"
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
            assert!(
                !sql.contains("dispatched_job"),
                "promotion is a readiness decision, not a dispatch one: {sql}"
            );
        }

        assert!(norm(&PROMOTE).contains("db.derivation = ANY($1::uuid[])"));
        assert!(
            norm(&PROMOTE_ANY).contains("(db.unready_deps = 0 OR db.substitutable) AND"),
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
    /// re-push between the loss and this call keeps its anchor queued - and a relay,
    /// whose arm of the gate never reads the `.drv`, is never pulled back by one.
    #[test]
    fn unpromoting_a_drv_owner_rechecks_the_whole_gate() {
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

    /// The generic un-promote is what every demand loss runs, so it must move
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
            log[0].contains("AND db.demanded"),
            "the embedded gate reads the demand column: {log:?}"
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

    /// The scope is the pending anchors and one edge past them, plus every row whose
    /// flag contradicts a column it carries, and it is SELECTed once rather than left
    /// as a subquery: an inline scope is re-evaluated per statement, so a row entering
    /// it after the lock would be written unlocked, which is the very ABA the lock
    /// exists to stop. The contradiction arms are what the wholeness recount leaves
    /// behind two hops below anything pending, and both are column-only tests so a
    /// partial index answers each.
    #[test]
    fn the_scope_reaches_one_edge_past_the_pending_anchors_and_every_contradicting_flag() {
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
        assert!(
            sql.contains("WHERE q.fetchable AND q.missing_runtime_deps > 0"),
            "a fetchable anchor with a runtime hole counted is a lie every walk trusts: {sql}"
        );
        assert!(
            sql.contains("WHERE q.status IN (3, 7) AND NOT q.fetchable"),
            "a terminal-success anchor without the flag may be whole again: {sql}"
        );
        assert!(
            !sql.contains("derivation_output"),
            "the scope reads columns only; the recount evaluates the predicate: {sql}"
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

        let scope = readiness_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let repaired = repair_readiness(&db, &scope).await.unwrap();

        assert_eq!((fetchable, repaired.unready_deps), (2, 3));
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
    /// comes out too LOW, and too low promotes and dispatches. The two are separate
    /// passes over one frozen scope so the sweep can put its demand recount between
    /// them.
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

        let scope = readiness_scope(&db).await.unwrap();
        let fetchable = repair_fetchable(&db, &scope).await.unwrap();
        let repaired = repair_readiness(&db, &scope).await.unwrap();

        assert_eq!(
            (fetchable, repaired.unready_deps),
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

    /// `Skipped` is the projection of "Created, and nothing wants it". An entry
    /// point is demand by definition, so a named root can never be settled by it,
    /// and the thaw goes back to `Created` rather than to the queue: the promote
    /// that follows is what reads the gates.
    #[test]
    fn skip_moves_only_a_created_undemanded_anchor_no_entry_point_names() {
        let sql = SKIP_UNDEMANDED.text();
        assert!(sql.contains("SET status = 10"), "{sql}");
        assert!(
            sql.contains("AND db.status = 0 AND NOT db.demanded"),
            "{sql}"
        );
        assert!(
            sql.contains(
                "NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation)"
            ),
            "{sql}"
        );

        let thaw = THAW_SKIPPED.text();
        assert!(
            thaw.contains("SET status = 0") && thaw.contains("AND db.status = 10 AND db.demanded"),
            "{thaw}"
        );
    }

    /// The recompute is absolute and writes only the rows that disagree, so its
    /// row count IS the drift and a healthy fleet writes nothing. It returns the
    /// new value per row, because one statement serves both directions: what it
    /// turned on is promoted and what it turned off is un-promoted.
    #[test]
    fn the_demand_recompute_writes_only_disagreeing_rows() {
        let sql = norm(RECOUNT_DEMANDED_SQL.as_str());
        assert!(
            sql.contains("WITH RECURSIVE demanded(evaluation, derivation, builder) AS"),
            "{sql}"
        );
        assert!(
            sql.contains("SET demanded = (db.derivation IN (SELECT derivation FROM demanded))"),
            "{sql}"
        );
        assert!(
            sql.contains("db.demanded <> (db.derivation IN (SELECT derivation FROM demanded))"),
            "rewriting agreeing rows reports drift that is not there: {sql}"
        );
        assert!(
            sql.contains("RETURNING db.derivation, db.demanded"),
            "the caller settles the queue from the new value: {sql}"
        );
    }

    /// The backstop is the last writer that can un-strand a subtree, so it writes
    /// every open anchor: a `Skipped` one, which demand returning thaws, and a
    /// `Completed` one with a hole in its closure, whose value seeds the bounded
    /// recompute below it. Restricted to a status list it read the walk's correct
    /// answer and then declined to apply it, twice over.
    #[test]
    fn the_backstop_rewrites_every_open_anchor() {
        let sql = norm(RECOUNT_DEMANDED_SQL.as_str());
        assert!(
            sql.contains(&format!(
                "WHERE {} AND db.demanded <>",
                norm(&open_predicate("db"))
            )),
            "{sql}"
        );
        assert!(
            sql.contains(&format!(
                "JOIN derivation w ON w.id = db.derivation WHERE {} UNION",
                norm(&open_predicate("db"))
            )),
            "a fetchable entry point seeds nothing: {sql}"
        );
    }

    /// Both directions, over a region that INCLUDES the roots. A thawed anchor's own
    /// demand is as stale as anything below it - its value was last written when it
    /// was terminal - so a recompute that only walked downward would relay busybox
    /// again the moment a retire reset it (#666). The seed comes from outside the
    /// region, because a member kept by an outside demander re-demands its own
    /// subtree. The walk answers and the write is handed what it answered.
    #[tokio::test]
    async fn the_bounded_recompute_covers_its_roots_and_seeds_from_outside() {
        let root = DerivationId::now_v7();
        let on = DerivationId::now_v7();
        let off = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0), exec(1)])
            .append_query_results([vec![demand_row(on, true), demand_row(off, false)]])
            .append_query_results([vec![demand_row(on, true), demand_row(off, false)]])
            .into_connection();

        let moved = recompute_demand(&db, &[root]).await.unwrap();
        assert_eq!(moved.gained, vec![on]);
        assert_eq!(moved.lost, vec![off]);

        let log = statements(db.into_transaction_log());
        assert!(
            log[0].contains("SET LOCAL work_mem")
                && log[1].contains("ORDER BY derivation FOR UPDATE"),
            "the walk its plan gate measures is raised and its roots locked: {log:?}"
        );
        let walk = norm(&log[2]);
        assert!(
            walk.contains("region(evaluation, derivation, builder) AS"),
            "{walk}"
        );
        assert!(
            walk.contains("p.derivation NOT IN (SELECT derivation FROM region)"),
            "the seed must come from demanders OUTSIDE the region: {walk}"
        );
        // The seed is a second expression of the walk's own step, so it stops
        // where the walk stops and nowhere else: an open, demanded parent, over a
        // runtime edge from anything and over any edge from a builder. A relay's
        // runtime references are demanded here too, and so are the references of
        // a `Completed` anchor whose closure has a hole.
        assert!(
            walk.contains(
                "WHERE e.dependency = r.derivation OFFSET 0) pe \
                 JOIN derivation_build p ON p.derivation = pe.parent \
                 JOIN derivation pw ON pw.id = p.derivation \
                 WHERE p.demanded AND (NOT p.fetchable AND p.status NOT IN (4, 5, 6, 9)) \
                 AND p.derivation NOT IN (SELECT derivation FROM region) \
                 AND ((pw.walked AND p.probed AND NOT p.substitutable \
                 AND p.status IN (0, 1, 2, 8)) OR pe.kind IN (1, 2)))"
            ),
            "{walk}"
        );
        assert!(
            walk.contains(
                "FROM region r JOIN derivation_build rb ON rb.derivation = r.derivation \
                 JOIN derivation w ON w.id = rb.derivation \
                 WHERE (NOT rb.fetchable AND rb.status NOT IN (4, 5, 6, 9)) \
                 AND (EXISTS (SELECT 1 FROM entry_point ep"
            ),
            "a settled root seeds nothing, and a seed carries its own builder bit: {walk}"
        );
        assert!(
            !walk.contains("build_job"),
            "a name is what adoption writes for what the walk reaches: {walk}"
        );
        assert!(
            walk.contains("FROM region r ORDER BY r.derivation"),
            "the write takes its locks in the order the walk sorted: {walk}"
        );
        let write = norm(&log[3]);
        assert!(
            write.contains("FROM unnest($1::uuid[], $2::bool[]) AS x(derivation, demanded)")
                && write.contains("RETURNING db.derivation, db.demanded"),
            "the region reaches the write as values, not as a subquery: {write}"
        );
    }
    /// The un-walk selects what was complete, drops the record, and only then locks
    /// anchors: derivation rows first, as every writer of them does.
    #[tokio::test]
    async fn an_unwalk_moves_the_counter_before_it_reaches_the_anchors() {
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
        let complete = at("AND walked AND unwalked_inputs = 0 ORDER BY id FOR UPDATE");
        let unwalk = at("UPDATE derivation SET walked = false");
        let anchors = at("FROM derivation_build");
        assert!(complete < unwalk && unwalk < anchors, "{log:?}");
    }
}
