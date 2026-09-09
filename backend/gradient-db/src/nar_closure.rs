/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `cached_path.missing_references`: how many of a path's references are not
//! whole. Seeded when the NAR is committed and moved by a frontier ripple over
//! `cached_path_reference` when a path becomes or stops being whole; nothing
//! re-derives it full-table, and [`repair_counters_for`] recomputes it only
//! over the paths a pending anchor gates on ([`gating_paths`]).
//! `whole_predicate` is the one definition every gate reads.
//!
//! Because the counter is moved and not recomputed, every ripple must be driven
//! by a TRANSITION, never by a state: rippling from a row that did not just flip, or
//! rippling one frontier twice, moves referrers past zero, and a negative
//! counter never satisfies `= 0` again. The ripples read that transition from
//! their own `RETURNING`; the seed cannot (see [`seed_references`]), so its
//! caller holds the pre-commit endpoint. Only [`repair_counters_for`] can
//! rescue a row driven negative, and it visits [`gating_paths`] alone: a path
//! driven negative while no anchor gates on it stays unwhole until something
//! does, holding every referrer unwhole with it.
//!
//! The repair is one level per sweep, not a fixpoint. A chunk recounts every
//! row from one snapshot and ripples nothing: two rows in one chunk may reference
//! each other, so a row's new value is computed against its reference's
//! PRE-repair counter, and rippling such a flip would drive the referrer past
//! zero. A repaired row's referrers therefore see the flip only by being recounted
//! themselves, so a chain of drifted rows converges one level per sweep interval,
//! and a referrer outside [`gating_paths`] never converges at all. Widening that
//! is #591's business.
//!
//! # Who writes it
//!
//! The graph actor handles one message at a time, so a commit never overlaps
//! another commit or one of the actor's own retires (`demote_cached_output` and
//! the demote passes around it). Three maintenance deletions run OUTSIDE the
//! actor, each in a transaction of its own: TTL eviction and the zombie purge in
//! `gradient_cache::cacher::cleanup`, and the orphan GC in [`crate::gc`]. Nothing
//! serialises those against a commit except the locks below, and
//! [`repair_counters_for`] is no backstop for them either - it visits
//! [`gating_paths`] alone.
//!
//! The ripple carries no visited set and no depth cap. Nix runtime references are
//! acyclic and both statements exclude self-references, so it terminates having
//! visited each row once. A cycle between two DISTINCT paths would also terminate
//! (the second visit fails the `= 0` / `= c.n` test) but leaves both counters
//! negative, which is the unrecoverable state above - the reference graph being a
//! DAG is a precondition, not an optimisation.
//!
//! # READ COMMITTED
//!
//! Every statement here runs under READ COMMITTED: its snapshot is taken at
//! statement start, BEFORE any lock wait, and EvalPlanQual re-checks the qual only
//! against the row version it is updating - it never re-evaluates a subquery over
//! another table. A statement that waited on a lock therefore still decides from
//! the world as it was before the wait, for everything except that one row. Three
//! consequences this module is built on: a decision that must see what committed
//! during the wait needs its own statement after it ([`retire_paths_where`]); a
//! counter moved relative to the row's own value is re-evaluated and so composes
//! with a concurrent move (both ripples); and an absolute recount has to hold its
//! rows BEFORE its snapshot opens ([`repair_counters_for`]), because a
//! compare-and-swap on the pre-image compares the fresh row against a snapshot
//! value that a concurrent seed can reproduce.
//!
//! # Lock order, and where it stops
//!
//! Every writer that takes row locks here takes them in ONE hash-ordered
//! statement before it decides anything: [`lock_reference_endpoints`] for a
//! commit, [`LOCK`] for either retire and for each chunk of the repair, which is
//! the third writer inside this discipline and not an exception to it. The two
//! absolute writers cannot skip it: [`seed_references`] runs only on a
//! [`ReferenceLock`] and the recount only on a `PathLock`, each constructible by
//! its lock function alone. The `ORDER BY hash` is not decoration.
//! With acquisition monotone in `hash`, a wait-for cycle would need some
//! transaction to wait on a lower hash than one it already holds; a single
//! unordered locker - a lock set that skips the row it is about to update, or a
//! `DELETE` taking its locks in scan order - deadlocks against however careful
//! the other side is, measured on the shape where a referrer's hash sorts after
//! its reference's.
//!
//! The ripples are outside that, and it is not closed. `FORWARD` and `REVERSE`
//! compute their referrer set inside the statement, so each takes
//! `FOR NO KEY UPDATE` on rows no ordered lock set covers, in whatever order its
//! plan produces. A commit and a concurrent maintenance retire can therefore
//! still deadlock. Postgres detects it rather than hanging: the retire logs and
//! retries on its next pass, and a killed commit fails a `NarUploaded` the worker
//! retries. That is the accepted price of `FOR SHARE` below - a detected, retried
//! deadlock instead of a permanently false-whole row.

use sea_orm::{
    ConnectionTrait, DatabaseBackend, DatabaseTransaction, DbErr, Statement, TransactionTrait,
};

/// The row `{alias}` is stored and every reference resolves to a whole row.
pub fn whole_predicate(alias: &str) -> String {
    format!("({alias}.file_hash IS NOT NULL AND {alias}.missing_references = 0)")
}

/// The value `missing_references` holds for referrer `{alias}`: the references,
/// self excluded, whose row is absent, unbacked or itself not whole. The seed and
/// the repair are the two halves of one invariant - if their counts ever
/// disagree, every sweep rewrites correct rows into incorrect ones - so both
/// compose this, and this composes [`whole_predicate`].
///
/// The count is per EDGE (`cached_path_reference` rows), not per distinct
/// reference hash, and both ripples move a referrer by `count(*)` over the same
/// edge rows. So two reference tokens sharing a store-path hash - which a
/// well-formed worker report cannot produce, but a hand-written narinfo on the
/// cache-upload endpoint could - are counted twice and cancelled twice, and
/// nothing here assumes hash-to-token injectivity. Counting distinct hashes here
/// would desynchronise the seed from the ripples, which is the one thing that
/// makes a sweep rewrite correct rows.
fn unwhole_reference_count(alias: &str) -> String {
    format!(
        "(SELECT count(*) FROM cached_path_reference r \
         LEFT JOIN cached_path dep ON dep.hash = r.reference_hash \
         WHERE r.referrer = {alias}.hash AND r.reference_hash <> {alias}.hash \
           AND NOT {whole})",
        whole = whole_predicate("dep"),
    )
}

fn seed_statement() -> String {
    format!(
        "UPDATE cached_path cp SET missing_references = {count} \
         WHERE cp.hash = $1 RETURNING {whole} AS whole",
        count = unwhole_reference_count("cp"),
        whole = whole_predicate("cp"),
    )
}

/// Serialising pass ahead of every delete, guarded or not: it is the retire's half
/// of the module doc's one hash-ordered acquisition, and `FOR UPDATE` conflicts
/// with the RI `FOR KEY SHARE` a concurrent `cached_path_signature` insert holds
/// on the parent row, so the wait for that insert is absorbed here and the DELETE
/// opens its snapshot after it (see the module doc on READ COMMITTED). It reads
/// nothing and decides nothing; see [`retire_paths`].
const LOCK: &str = "SELECT 1 FROM cached_path WHERE hash = ANY($1) ORDER BY hash FOR UPDATE";

/// The rows a commit's seed can count from: the references it reports (tokens,
/// so the hash is their prefix), the references currently indexed for it, and its
/// own row. See [`lock_reference_endpoints`].
const LOCK_REFERENCES: &str = "\
    SELECT 1 FROM cached_path \
    WHERE hash IN (SELECT split_part(t.tok, '-', 1) FROM unnest($1::text[]) AS t(tok) WHERE t.tok <> '' \
                   UNION \
                   SELECT reference_hash FROM cached_path_reference WHERE referrer = $2 \
                   UNION \
                   SELECT $2::text) \
    ORDER BY hash FOR SHARE";

/// Lock every row a commit's counter will be counted from, before the commit
/// decides anything.
///
/// [`seed_references`] counts `cached_path` rows under its own READ COMMITTED
/// snapshot. A maintenance retire deleting one of them from another transaction
/// computes its referrers from `cached_path_reference` in a snapshot that cannot
/// see an edge this commit has not committed yet, so it never increments this
/// path, while the seed counts a reference that is already gone: the row ends
/// whole with a dangling edge, permanently, and no fixpoint re-derives it any
/// more. The dispatch gate reads that as a complete closure and sends a build
/// against a missing input.
///
/// `FOR SHARE` is the weakest strength that works. It conflicts with the `DELETE`
/// in [`retire_paths`], so the retire waits for this transaction and its reverse
/// ripple - a separate statement, hence a fresh snapshot - then sees the new
/// edge. It also conflicts with the ripples' `FOR NO KEY UPDATE`, which
/// `FOR KEY SHARE` does not: without that, a retire deeper in the closure can
/// unwhole one of these references between this seed's read and its commit, and
/// the ripple that should have carried the loss up to this path runs before the
/// edge exists. Measured on a three-level chain: `FOR KEY SHARE` leaves this row
/// whole with an unwhole reference, `FOR SHARE` does not. It still conflicts with
/// nothing a commit needs - another commit's share lock and the RI locks a
/// `cached_path_signature` insert takes are both compatible.
///
/// Runs BEFORE `upsert_cached_path`'s `FOR UPDATE` and includes the referrer's
/// own row, so this is the single hash-ordered acquisition the module doc
/// requires: the later `FOR UPDATE` only strengthens a lock this transaction
/// already holds, which cannot introduce a wait. It takes the transaction rather
/// than a `WorkerDb` for the same reason [`retire_paths_where`] does - on a
/// pooled handle every lock is released at the end of the statement that took it.
pub async fn lock_reference_endpoints<'txn>(
    txn: &'txn DatabaseTransaction,
    hash: &str,
    references: &[String],
) -> Result<ReferenceLock<'txn>, DbErr> {
    txn.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        LOCK_REFERENCES,
        [references.to_vec().into(), hash.into()],
    ))
    .await?;

    Ok(ReferenceLock {
        txn,
        hash: hash.to_owned(),
    })
}

/// Proof that a commit holds its reference endpoints: the row it will seed and
/// every row the seed can count from, `FOR SHARE`, in one hash-ordered statement
/// on `txn`. Only [`lock_reference_endpoints`] constructs one and
/// [`seed_references`] accepts nothing else, so a seed cannot run unlocked,
/// outside the locking transaction, or on a row the lock did not name. What the
/// type cannot enforce is that the caller locked BEFORE opening the snapshot it
/// decides from: a writer that ran its own SELECT first still compiles. That gap
/// is closed only by the seed recounting inside itself, from this proof, which is
/// why it takes no other input.
#[must_use = "a lock proves nothing unless the seed runs on it"]
pub struct ReferenceLock<'txn> {
    txn: &'txn DatabaseTransaction,
    hash: String,
}

const FORWARD: &str = r#"
    UPDATE cached_path cp
    SET missing_references = cp.missing_references - c.n
    FROM (SELECT r.referrer, count(*) AS n FROM cached_path_reference r
          WHERE r.reference_hash = ANY($1) AND r.referrer <> r.reference_hash
          GROUP BY r.referrer) c
    WHERE cp.hash = c.referrer
    RETURNING cp.hash, (cp.file_hash IS NOT NULL AND cp.missing_references = 0) AS whole
"#;

const REVERSE: &str = r#"
    UPDATE cached_path cp
    SET missing_references = cp.missing_references + c.n
    FROM (SELECT r.referrer, count(*) AS n FROM cached_path_reference r
          WHERE r.reference_hash = ANY($1) AND r.referrer <> r.reference_hash
          GROUP BY r.referrer) c
    WHERE cp.hash = c.referrer
    RETURNING cp.hash, (cp.file_hash IS NOT NULL AND cp.missing_references = c.n) AS was_whole
"#;

fn delete_statement(guard: Option<&str>) -> String {
    let guard = guard.map(|g| format!(" AND ({g})")).unwrap_or_default();
    format!(
        "DELETE FROM cached_path cp WHERE cp.hash = ANY($1){guard} \
         RETURNING cp.hash, {whole} AS was_whole",
        whole = whole_predicate("cp"),
    )
}

/// Recount the row the lock names from its references and report whether the
/// row IS whole afterwards (`false` when no such row exists).
///
/// This is a STATE, and the ripples need a TRANSITION, so the caller owns the
/// other endpoint: the row's wholeness BEFORE the commit
/// ([`gradient_entity::cached_path::Model::is_whole`] on the row the commit read
/// before writing it), and it ripples only when the two differ. The statement
/// cannot report that endpoint itself - by the time it runs, the commit has
/// already stored the NAR, so a `FROM cached_path old` self-join sees a row that
/// is backed and, on a fresh insert, counts zero: whole. Reading the flip from
/// that would report `false` for exactly the commit that must ripple, and every
/// referrer of a re-pushed path would count it as missing forever.
pub async fn seed_references(lock: &ReferenceLock<'_>) -> Result<bool, DbErr> {
    let Some(row) = lock
        .txn
        .query_one_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            seed_statement(),
            [lock.hash.as_str().into()],
        ))
        .await?
    else {
        return Ok(false);
    };

    row.try_get::<bool>("", "whole")
}

/// One level per statement: decrement the referrers of the frontier and
/// continue from those that reached zero. Returns every hash that became
/// whole, the seeds included.
///
/// Every seed must be a row that JUST became whole (see [`seed_references`]),
/// and no seed may reference another seed: the statement decrements a referrer
/// once per edge into the batch, so a seed that was already whole, or one whose
/// referrer is itself a seed, drives that referrer's counter below zero.
pub async fn ripple_whole<C: ConnectionTrait>(
    db: &C,
    became_whole: Vec<String>,
) -> Result<Vec<String>, DbErr> {
    ripple(db, FORWARD, "whole", became_whole).await
}

/// The reverse: increment the referrers of the frontier and continue from
/// those that were whole before. Returns every hash that stopped being whole,
/// the seeds included.
///
/// The mirror precondition holds: every seed must be a row that JUST stopped
/// being whole, and no seed may reference another seed, or a referrer is
/// incremented twice for one loss.
pub async fn ripple_unwhole<C: ConnectionTrait>(
    db: &C,
    stopped_being_whole: Vec<String>,
) -> Result<Vec<String>, DbErr> {
    ripple(db, REVERSE, "was_whole", stopped_being_whole).await
}

async fn ripple<C: ConnectionTrait>(
    db: &C,
    statement: &str,
    flag: &str,
    seeds: Vec<String>,
) -> Result<Vec<String>, DbErr> {
    let mut all = seeds.clone();
    let mut frontier = seeds;
    while !frontier.is_empty() {
        let rows = db
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                statement,
                [frontier.into()],
            ))
            .await?;

        let mut next = Vec::new();
        for row in rows {
            if row.try_get::<bool>("", flag)? {
                next.push(row.try_get::<String>("", "hash")?);
            }
        }

        all.extend(next.iter().cloned());
        frontier = next;
    }

    Ok(all)
}

/// What `retire_paths` removed and what stopped being whole because of it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Retired {
    pub deleted: Vec<String>,
    pub unwhole: Vec<String>,
}

/// Delete `hashes` from the index and move every counter and flag that trusted
/// them, in the caller's transaction: the reverse ripple from the rows that were
/// whole, `is_cached` off the deleted outputs, and the anchor flags
/// (`drv_closure_cached` on the owner of a deleted or unwholed `.drv`,
/// `closure_complete` on its producers) off every hash that was DELETED or that
/// stopped being whole. `Retired::deleted` names the rows that actually went. Use
/// [`retire_paths_where`] when the caller may drop a path only while some
/// condition still holds.
///
/// The anchor flags follow the union and not just the unwhole set because neither
/// reads the counter: `drv_closure_cached` requires the `.drv`'s own row to be
/// backed plus the dependency recursion, and the Completed arm of
/// `closure_complete` only that every output has a backed `cached_path`. So a row
/// that was deleted while it was NOT whole would keep a stale-true gate until the
/// next `Global` reconcile clears it, and `find_ready_anchors` runs every 5s: six
/// dispatch passes able to send a build against a `.drv` no longer in the cache.
///
/// Every retire opens with the [`LOCK`] pass, so it must run inside a transaction:
/// a `DELETE` on its own acquires in scan order, and one unordered locker
/// deadlocks against the hash-ordered acquisition every other writer makes (see
/// the module doc). Every caller does - the actor's transaction for a demote, a
/// `begin()` per batch for the zombie purge, per chunk for the orphan GC - and the
/// connection stays generic only because the unguarded form takes no decision from
/// the lock; [`retire_paths_where`] does, and its type says so.
pub async fn retire_paths<C: ConnectionTrait>(db: &C, hashes: &[String]) -> Result<Retired, DbErr> {
    retire(db, hashes, None).await
}

/// [`retire_paths`] restricted to the rows that still satisfy `guard`, a
/// predicate over the row being deleted (aliased `cp`). The TTL eviction passes
/// `NOT EXISTS (SELECT 1 FROM cached_path_signature ...)`, since
/// `cached_path_signature.cached_path` is `ON DELETE CASCADE` and a retire takes
/// every cache's signature with it.
///
/// Two statements, and neither is sufficient alone. The guard has to be evaluated
/// by the DELETE, because a preceding statement would decide from its own snapshot;
/// and the DELETE has to start only once the wait for an in-flight signature insert
/// is over, which is the module doc's READ COMMITTED argument. Guard alone: the
/// DELETE blocks on the insert's RI `FOR KEY SHARE` lock, proceeds without ever
/// seeing the signature that committed meanwhile, and cascades it away, 404ing a
/// narinfo committed seconds earlier. [`LOCK`] alone: nothing decides. Together the
/// wait is absorbed in its own statement and the DELETE opens a fresh snapshot that
/// sees the signature its guard then tests.
///
/// That is why this takes a `&DatabaseTransaction` and not a connection: the locks
/// [`LOCK`] takes have to still be held when the DELETE runs, and on a pooled
/// connection each statement is its own implicit transaction, so every one of them
/// would be released first and the race would be back with nothing to notice it.
/// The type is the enforcement - a pooled guarded retire does not compile.
pub async fn retire_paths_where(
    txn: &DatabaseTransaction,
    hashes: &[String],
    guard: &str,
) -> Result<Retired, DbErr> {
    retire(txn, hashes, Some(guard)).await
}

async fn retire<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
    guard: Option<&str>,
) -> Result<Retired, DbErr> {
    if hashes.is_empty() {
        return Ok(Retired::default());
    }

    db.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        LOCK,
        [hashes.to_vec().into()],
    ))
    .await?;

    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            delete_statement(guard),
            [hashes.to_vec().into()],
        ))
        .await?;

    let mut deleted = Vec::with_capacity(rows.len());
    let mut were_whole = Vec::new();
    for row in rows {
        let hash = row.try_get::<String>("", "hash")?;
        if row.try_get::<bool>("", "was_whole")? {
            were_whole.push(hash.clone());
        }

        deleted.push(hash);
    }

    let unwhole = ripple_unwhole(db, were_whole).await?;
    let seen: std::collections::HashSet<String> = deleted.iter().cloned().collect();
    let mut gated = deleted.clone();
    gated.extend(unwhole.iter().filter(|h| !seen.contains(*h)).cloned());

    if !deleted.is_empty() {
        db.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "UPDATE derivation_output SET is_cached = false WHERE is_cached AND hash = ANY($1)",
            [deleted.clone().into()],
        ))
        .await?;
    }

    if !gated.is_empty() {
        db.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE derivation_build db SET drv_closure_cached = false
            FROM derivation d
            WHERE d.id = db.derivation AND db.drv_closure_cached AND d.hash = ANY($1)
            "#,
            [gated.clone().into()],
        ))
        .await?;

        db.execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"
            UPDATE derivation_build db SET closure_complete = false
            WHERE db.closure_complete
              AND db.derivation IN (SELECT o.derivation FROM derivation_output o WHERE o.hash = ANY($1))
            "#,
            [gated.into()],
        ))
        .await?;
    }

    Ok(Retired { deleted, unwhole })
}

fn repair_statement() -> String {
    format!(
        "UPDATE cached_path cp SET missing_references = x.n \
         FROM (SELECT c.hash, c.missing_references AS old, {count} AS n \
               FROM cached_path c WHERE c.hash = ANY($1)) x \
         WHERE cp.hash = x.hash AND cp.missing_references = x.old AND x.old <> x.n",
        count = unwhole_reference_count("c"),
    )
}

/// Recompute the counter for `hashes` from their references and write the rows
/// that disagree. Returns how many were repaired. The ripples move the counter
/// rather than derive it, so this is the bounded backstop against a move that
/// was lost (a crashed transaction, a hand-edited row), never a sweep.
///
/// Each chunk is one transaction that runs the [`LOCK`] pass first and recounts
/// second, so the recount's snapshot opens only after every in-flight commit of
/// those rows has finished. Recounting first and comparing-and-swapping on the
/// pre-image is not enough: the CAS reads the fresh row, but `x.old` and `x.n`
/// come from the statement's snapshot, so a commit that reseeds the row
/// absolutely onto the drifted value passes it and is overwritten with a count
/// over the pre-commit edge set - a false-whole row for one sweep interval,
/// reproduced on Postgres 18. The CAS stays anyway: under the lock it compares
/// the row to itself, but a caller that loses the lock degrades to a skipped row
/// instead of an unconditional overwrite of a counter nothing else re-derives,
/// and `x.old <> x.n` is the drift filter behind the returned count. Chunked so
/// each chunk commits on its own: as one statement over a fleet evaluation's
/// gating set the sweep's budget cancels it in place and rolls back every repair,
/// silently.
pub async fn repair_counters_for<C>(db: &C, hashes: &[String]) -> Result<u64, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut repaired = 0u64;
    for chunk in hashes.chunks(crate::IN_CHUNK_SIZE) {
        let txn = db.begin().await?;
        let lock = lock_paths(&txn, chunk).await?;
        repaired += recount(&lock).await?;
        txn.commit().await?;
    }

    Ok(repaired)
}

/// Proof that one repair chunk is held `FOR UPDATE`, hash-ordered, on `txn`.
/// Only [`lock_paths`] constructs one and [`recount`] writes exactly the rows it
/// carries, so a recount cannot run unlocked, in another transaction, or over
/// rows the lock did not cover. The caveat on [`ReferenceLock`] applies here too:
/// the proof says the write follows the lock, not that no read preceded it.
#[must_use = "a lock proves nothing unless the recount runs on it"]
struct PathLock<'txn> {
    txn: &'txn DatabaseTransaction,
    hashes: Vec<String>,
}

async fn lock_paths<'txn>(
    txn: &'txn DatabaseTransaction,
    hashes: &[String],
) -> Result<PathLock<'txn>, DbErr> {
    txn.execute_raw(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        LOCK,
        [hashes.to_vec().into()],
    ))
    .await?;

    Ok(PathLock {
        txn,
        hashes: hashes.to_vec(),
    })
}

/// Recount the locked rows and write the ones that disagree, returning how many.
async fn recount(lock: &PathLock<'_>) -> Result<u64, DbErr> {
    Ok(lock
        .txn
        .execute_raw(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            repair_statement(),
            [lock.hashes.clone().into()],
        ))
        .await?
        .rows_affected())
}

/// The paths the pending anchors gate on: their own `.drv` rows and the output
/// rows of their direct dependencies. This is what bounds [`repair_counters_for`],
/// so every path a DISPATCH gate reads is repaired on each sweep. An eval-time
/// prune reads a wider set and is NOT repaired: `gradient_graph::known::prunable`
/// and the scheduler's substitutability pass ask whether the outputs of arbitrary
/// walked candidates are whole, and most of those have no pending anchor. A
/// false-whole there prunes a subtree that is then never walked, recorded or
/// built - a permanent dead end, not a stall a later build clears.
pub fn gating_paths() -> String {
    let pending = crate::status_sql::build_in(&[
        gradient_entity::build::BuildStatus::Created,
        gradient_entity::build::BuildStatus::Queued,
    ]);
    format!(
        r#"
    SELECT d.hash FROM derivation d
    JOIN derivation_build db ON db.derivation = d.id
    WHERE db.status IN ({pending})
  UNION
    SELECT o.hash FROM derivation_output o
    JOIN derivation_dependency e ON e.dependency = o.derivation
    JOIN derivation_build db ON db.derivation = e.derivation
    WHERE db.status IN ({pending})
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, TransactionTrait, Value};
    use std::collections::BTreeMap;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn row(hash: &str, flag: &str, value: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("hash".to_owned(), Value::from(hash.to_owned())),
            (flag.to_owned(), Value::from(value)),
        ])
    }

    fn flag_row(flag: &str, value: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([(flag.to_owned(), Value::from(value))])
    }

    fn exec(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    /// A reference counts as missing when its row is absent, unbacked or not
    /// whole; a self-reference never counts, or a self-referential path could
    /// never be whole.
    #[test]
    fn the_seed_counts_absent_unbacked_and_unwhole_references_but_not_self() {
        let sql = norm(&seed_statement());
        assert!(sql.contains("LEFT JOIN cached_path dep ON dep.hash = r.reference_hash"));
        assert!(sql.contains("r.reference_hash <> cp.hash"));
        assert!(
            sql.contains(&format!("AND NOT {}", whole_predicate("dep"))),
            "{sql}"
        );
        assert_eq!(
            norm(&whole_predicate("cp")),
            "(cp.file_hash IS NOT NULL AND cp.missing_references = 0)"
        );
    }

    /// The seed reports the state after the recount and nothing else: the commit
    /// has already stored the NAR by then, so no self-join can recover the
    /// pre-commit endpoint of the flip. Reading the flip from the row itself
    /// would report "did not become whole" for exactly the commit that must
    /// ripple (a fresh row is inserted backed, with the counter at its default
    /// zero), and every referrer of a re-pushed path would count it as missing
    /// forever. The caller pairs this with the pre-commit `is_whole()`.
    #[tokio::test]
    async fn the_seed_reports_the_state_and_leaves_the_transition_to_its_caller() {
        let sql = norm(&seed_statement());
        assert!(!sql.contains("cached_path old"), "{sql}");
        assert!(sql.contains("WHERE cp.hash = $1"), "{sql}");
        assert!(
            sql.contains(&format!("RETURNING {} AS whole", whole_predicate("cp"))),
            "{sql}"
        );

        for whole in [true, false] {
            let db = MockDatabase::new(DatabaseBackend::Postgres)
                .append_query_results([vec![flag_row("whole", whole)]])
                .append_exec_results([exec(0)])
                .into_connection();
            let txn = db.begin().await.unwrap();
            let lock = lock_reference_endpoints(&txn, "h", &[]).await.unwrap();

            assert_eq!(seed_references(&lock).await.unwrap(), whole);
        }

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(0)])
            .into_connection();
        let txn = db.begin().await.unwrap();
        let lock = lock_reference_endpoints(&txn, "absent", &[]).await.unwrap();

        assert!(!seed_references(&lock).await.unwrap());
    }

    /// The seed takes the proof and nothing else, so the row it recounts is the
    /// row the lock named and the statement runs on the locking transaction: a
    /// caller cannot seed a row it did not lock, or seed after the lock is gone.
    #[tokio::test]
    async fn the_seed_recounts_only_the_row_its_lock_names() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![flag_row("whole", true)]])
            .append_exec_results([exec(0)])
            .into_connection();
        let txn = db.begin().await.unwrap();
        let lock = lock_reference_endpoints(&txn, "h", &["d-dep".to_owned()])
            .await
            .unwrap();
        seed_references(&lock).await.unwrap();
        drop(lock);
        txn.commit().await.unwrap();

        let raw = db.into_transaction_log();
        assert_eq!(raw.len(), 1, "lock and seed share one transaction: {raw:?}");
        let log = crate::pool::statements(raw);
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(
            log[1].contains("SET missing_references = (") && log[1].contains("\"h\""),
            "the seed recounts the locked row: {log:?}"
        );
    }

    /// The commit's lock covers every row its seed can count from - the reported
    /// tokens' hashes, the currently indexed references, and the referrer's own row
    /// - in one hash-ordered statement. Self is included on purpose: a referrer
    /// that locked its references but not itself acquires out of hash order when
    /// its own hash sorts higher, and deadlocks against a bulk retire that reached
    /// it first. `FOR SHARE` conflicts with the retiring DELETE and with the
    /// ripples' non-key UPDATE, and with nothing a concurrent commit or
    /// signature insert needs.
    #[tokio::test]
    async fn the_commit_locks_every_reference_endpoint_in_hash_order() {
        let sql = norm(LOCK_REFERENCES);
        assert!(
            sql.contains("split_part(t.tok, '-', 1) FROM unnest($1::text[])"),
            "the reported tokens: {sql}"
        );
        assert!(
            sql.contains("SELECT reference_hash FROM cached_path_reference WHERE referrer = $2"),
            "the currently indexed references: {sql}"
        );
        assert!(
            sql.contains("UNION SELECT $2::text"),
            "the referrer's own row: {sql}"
        );
        assert!(sql.ends_with("ORDER BY hash FOR SHARE"), "{sql}");
        assert!(
            !sql.contains("FOR UPDATE"),
            "a commit must not block another commit: {sql}"
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();

        let txn = db.begin().await.unwrap();
        let lock = lock_reference_endpoints(&txn, "h", &["d-dep".to_owned()])
            .await
            .unwrap();
        assert_eq!(lock.hash, "h", "the proof names the row the seed may write");
        drop(lock);
        txn.commit().await.unwrap();

        let log = crate::pool::statements(db.into_transaction_log());
        assert_eq!(log.len(), 1, "one statement, one acquisition: {log:?}");
        assert!(
            log[0].contains("\"h\"") && log[0].contains("\"d-dep\""),
            "{log:?}"
        );
    }

    /// Both ripples move a referrer once per edge into the frontier, so the
    /// per-referrer edge count and the self-exclusion are what keeps the counter
    /// sound.
    #[test]
    fn both_ripples_count_edges_per_referrer_and_exclude_self_references() {
        for sql in [norm(FORWARD), norm(REVERSE)] {
            assert!(
                sql.contains("count(*) AS n FROM cached_path_reference r"),
                "{sql}"
            );
            assert!(sql.contains("r.reference_hash = ANY($1)"), "{sql}");
            assert!(sql.contains("r.referrer <> r.reference_hash"), "{sql}");
            assert!(sql.contains("GROUP BY r.referrer"), "{sql}");
            assert!(sql.contains("WHERE cp.hash = c.referrer"), "{sql}");
        }

        assert!(norm(FORWARD).contains("SET missing_references = cp.missing_references - c.n"));
        assert!(norm(REVERSE).contains("SET missing_references = cp.missing_references + c.n"));
    }

    /// `RETURNING` sees the row after the update, so the seed, the forward ripple
    /// and the delete return the wholeness predicate itself, while the reverse
    /// ripple compares against `c.n`: the counter equals what was just added
    /// exactly when it was zero before, i.e. when the row was whole.
    #[test]
    fn every_returning_predicate_reads_the_one_wholeness_definition() {
        assert!(
            norm(&seed_statement())
                .contains(&format!("RETURNING {} AS whole", whole_predicate("cp")))
        );
        assert!(norm(FORWARD).contains(&format!(
            "RETURNING cp.hash, {} AS whole",
            whole_predicate("cp")
        )));
        assert!(norm(&delete_statement(None)).contains(&format!(
            "RETURNING cp.hash, {} AS was_whole",
            whole_predicate("cp")
        )));
        assert!(norm(REVERSE).contains(
            "RETURNING cp.hash, (cp.file_hash IS NOT NULL AND cp.missing_references = c.n) AS was_whole"
        ));
    }

    /// The forward ripple decrements every referrer of the frontier once per
    /// reference and carries on only from referrers that reached zero.
    #[tokio::test]
    async fn the_forward_ripple_continues_only_from_rows_that_became_whole() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("r1", "whole", true), row("r2", "whole", false)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let whole = ripple_whole(&db, vec!["seed".to_owned()]).await.unwrap();

        assert_eq!(whole, vec!["seed".to_owned(), "r1".to_owned()]);
        let log = crate::pool::statements(db.into_transaction_log());
        assert_eq!(log.len(), 2, "one statement per level: {log:?}");
        assert!(log[0].contains("missing_references - c.n") && log[0].contains("\"seed\""));
        assert!(
            log[1].contains("\"r1\"") && !log[1].contains("\"r2\""),
            "{log:?}"
        );
    }

    /// The reverse ripple increments and carries on only from referrers that
    /// were whole before the increment.
    #[tokio::test]
    async fn the_reverse_ripple_continues_only_from_rows_that_were_whole() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                row("r1", "was_whole", true),
                row("r2", "was_whole", false),
            ]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        let unwhole = ripple_unwhole(&db, vec!["gone".to_owned()]).await.unwrap();

        assert_eq!(unwhole, vec!["gone".to_owned(), "r1".to_owned()]);
        let log = crate::pool::statements(db.into_transaction_log());
        assert_eq!(log.len(), 2, "one statement per level: {log:?}");
        assert!(log[0].contains("missing_references + c.n") && log[0].contains("\"gone\""));
        assert!(
            log[1].contains("\"r1\"") && !log[1].contains("\"r2\""),
            "{log:?}"
        );
    }

    /// A decode failure must not read as "not whole" or as "nothing here": that
    /// would truncate the ripple and leave referrers unwhole forever, with no sweep
    /// behind it. Every row this module reads propagates - the ripple's flag and its
    /// hash, the seed's flag, and the delete's two.
    #[tokio::test]
    async fn a_row_that_does_not_decode_is_an_error_not_a_dead_end() {
        let ripple_flag = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![row("r1", "misspelled", true)]])
            .into_connection();
        assert!(
            ripple_whole(&ripple_flag, vec!["seed".to_owned()])
                .await
                .is_err()
        );

        let ripple_hash = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([
                ("misspelled".to_owned(), Value::from("r1".to_owned())),
                ("whole".to_owned(), Value::from(true)),
            ])]])
            .into_connection();
        assert!(
            ripple_whole(&ripple_hash, vec!["seed".to_owned()])
                .await
                .is_err(),
            "the frontier's next level must not silently lose a row"
        );

        let seed_flag = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![flag_row("misspelled", true)]])
            .append_exec_results([exec(0)])
            .into_connection();
        let txn = seed_flag.begin().await.unwrap();
        let lock = lock_reference_endpoints(&txn, "h", &[]).await.unwrap();
        assert!(
            seed_references(&lock).await.is_err(),
            "an undecodable seed must not report the row as unwhole"
        );

        let delete_flag = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([(
                "hash".to_owned(),
                Value::from("a".to_owned()),
            )])]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .into_connection();
        assert!(
            retire_paths(&delete_flag, &["a".to_owned()]).await.is_err(),
            "a delete whose wholeness flag does not decode must not skip the ripple"
        );
    }

    /// Retiring seeds the reverse ripple only from rows that were whole: a
    /// referrer of a row that was already incomplete counted it as missing
    /// already, so it must not be incremented twice. The flag clears do NOT
    /// split that way: `is_cached` follows what was deleted, and the anchor flags
    /// follow the union of what was deleted and what stopped being whole, because
    /// neither flag reads the counter - a row deleted while it was not whole (`b`
    /// here) would otherwise keep a stale-true gate until the next `Global`
    /// reconcile and dispatch a build against a `.drv` that is gone. The unguarded
    /// retire opens with the same hash-ordered lock pass as the guarded one: it
    /// decides nothing there, but an unordered acquisition deadlocks against every
    /// other writer's ordered one.
    #[tokio::test]
    async fn retire_clears_the_anchor_flags_for_every_retired_path() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                row("a", "was_whole", true),
                row("b", "was_whole", false),
            ]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                4
            ])
            .into_connection();

        let retired = retire_paths(&db, &["a".to_owned(), "b".to_owned()])
            .await
            .unwrap();

        assert_eq!(retired.deleted, vec!["a".to_owned(), "b".to_owned()]);
        assert_eq!(retired.unwhole, vec!["a".to_owned()]);
        let log = crate::pool::statements(db.into_transaction_log());
        assert_eq!(
            log.len(),
            6,
            "lock, delete, one ripple level, three flag clears: {log:?}"
        );
        assert!(log[0].contains("FOR UPDATE") && !log[0].contains("DELETE"));
        assert!(log[1].contains("DELETE FROM cached_path") && log[1].contains("RETURNING"));
        assert!(
            log[2].contains("missing_references + c.n")
                && log[2].contains("\"a\"")
                && !log[2].contains("\"b\"")
        );
        assert!(
            log[3].contains("is_cached = false")
                && log[3].contains("\"a\"")
                && log[3].contains("\"b\""),
            "is_cached follows every deleted hash: {log:?}"
        );
        for clear in &log[4..] {
            assert!(
                clear.contains("\"a\"") && clear.contains("\"b\""),
                "the anchor flags follow every retired path, whole or not: {log:?}"
            );
        }

        assert!(log[4].contains("drv_closure_cached = false"));
        assert!(log[5].contains("closure_complete = false"));
    }

    /// Nothing to retire is a no-op: no statement at all.
    #[tokio::test]
    async fn retiring_nothing_issues_no_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        let retired = retire_paths(&db, &[]).await.unwrap();
        assert!(retired.deleted.is_empty() && retired.unwhole.is_empty());
        assert!(crate::pool::statements(db.into_transaction_log()).is_empty());
    }

    /// Both halves of a guarded retire are load-bearing, for the reason the module
    /// doc gives once: the guard must be evaluated by the DELETE, and the DELETE
    /// must open its snapshot only after the wait for an in-flight signature insert
    /// is over, or it cascades away a signature that committed while it waited.
    ///
    /// The lock has to outlive its own statement for that to hold, which is why
    /// this drives a transaction: `retire_paths_where` takes a
    /// `&DatabaseTransaction`, so the pooled form the mock cannot distinguish is
    /// rejected by the compiler instead.
    #[tokio::test]
    async fn a_guarded_retire_serialises_first_and_decides_in_the_delete() {
        let guard =
            "NOT EXISTS (SELECT 1 FROM cached_path_signature s WHERE s.cached_path = cp.id)";
        let sql = norm(&delete_statement(Some(guard)));
        assert!(
            sql.contains(&format!("WHERE cp.hash = ANY($1) AND ({guard})")),
            "the guard decides inside the delete: {sql}"
        );
        assert!(
            norm(LOCK).contains("ORDER BY hash FOR UPDATE"),
            "the lock pass serialises, ordered so two evictions cannot deadlock: {LOCK}"
        );
        assert!(
            !norm(LOCK).contains("cached_path_signature"),
            "the lock pass must decide nothing: {LOCK}"
        );

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                };
                4
            ])
            .into_connection();

        let txn = db.begin().await.unwrap();
        retire_paths_where(&txn, &["a".to_owned()], guard)
            .await
            .unwrap();
        txn.commit().await.unwrap();

        let log = crate::pool::statements(db.into_transaction_log());
        assert!(
            log[0].contains("FOR UPDATE") && !log[0].contains("DELETE"),
            "the wait is absorbed before the delete: {log:?}"
        );
        assert!(
            log[1].contains("DELETE FROM cached_path") && log[1].contains("cached_path_signature"),
            "the guard must reach the delete: {log:?}"
        );
        assert_eq!(
            log.len(),
            2,
            "a guard that spared every row leaves nothing to clear: {log:?}"
        );
    }

    /// The seed and the repair are the two halves of one invariant: if their
    /// counts ever disagree, every sweep rewrites correct rows into incorrect ones
    /// and reports permanent drift, and nothing else in the system would notice.
    #[test]
    fn the_seed_and_the_repair_count_references_identically() {
        let seed = norm(&seed_statement());
        let repair = norm(&repair_statement());
        assert!(
            seed.contains(&norm(&unwhole_reference_count("cp"))),
            "{seed}"
        );
        assert!(
            repair.contains(&norm(&unwhole_reference_count("c"))),
            "{repair}"
        );
        for sql in [&seed, &repair] {
            assert!(
                sql.contains(&format!("AND NOT {}", whole_predicate("dep"))),
                "both must count against the one wholeness definition: {sql}"
            );
        }
    }

    /// The compare-and-swap is the degradation guard, not the correctness
    /// argument (the lock is): a caller that loses the lock must skip a row that
    /// moved rather than overwrite a counter nothing else re-derives, and
    /// `x.old <> x.n` is the drift filter behind the returned count.
    #[test]
    fn the_repair_compares_and_swaps_on_the_pre_image() {
        let sql = norm(&repair_statement());
        assert!(sql.contains("c.missing_references AS old"), "{sql}");
        assert!(
            sql.contains(
                "WHERE cp.hash = x.hash AND cp.missing_references = x.old AND x.old <> x.n"
            ),
            "a row that moved under us must be skipped, not overwritten: {sql}"
        );
    }

    /// The recount's snapshot must open after the rows are held: recounting
    /// first and comparing-and-swapping on the pre-image lets a commit that
    /// reseeds the row onto the drifted value slip through, and the stale count
    /// then overwrites the seed. So each chunk is one transaction whose first
    /// statement is the hash-ordered lock pass and whose second is the recount.
    #[tokio::test]
    async fn the_repair_locks_the_chunk_before_it_recounts() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
            ])
            .into_connection();

        assert_eq!(
            repair_counters_for(&db, &["b".to_owned(), "a".to_owned()])
                .await
                .unwrap(),
            1
        );
        let raw = db.into_transaction_log();
        assert_eq!(raw.len(), 1, "one transaction per chunk: {raw:?}");
        let log = crate::pool::statements(raw);
        assert_eq!(log.len(), 2, "lock, then recount: {log:?}");
        assert!(
            log[0].contains("ORDER BY hash FOR UPDATE") && !log[0].contains("missing_references"),
            "the lock pass decides nothing: {log:?}"
        );
        assert!(
            log[1].contains("SET missing_references = x.n"),
            "the recount runs under the lock: {log:?}"
        );
        for statement in &log {
            assert!(
                statement.contains("\"a\"") && statement.contains("\"b\""),
                "both statements cover the whole chunk: {log:?}"
            );
        }
    }

    /// Unchunked over a fleet evaluation's gating set the sweep's budget cancels
    /// the statement in place, rolls back every repair and makes zero progress,
    /// silently, forever. One transaction per chunk means each chunk commits.
    #[tokio::test]
    async fn the_repair_commits_one_chunk_at_a_time() {
        let hashes: Vec<String> = (0..crate::IN_CHUNK_SIZE + 1)
            .map(|i| format!("h{i}"))
            .collect();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 3,
                };
                4
            ])
            .into_connection();

        assert_eq!(repair_counters_for(&db, &hashes).await.unwrap(), 6);
        let raw = db.into_transaction_log();
        assert_eq!(raw.len(), 2, "one transaction per chunk: {raw:?}");
        assert_eq!(
            crate::pool::statements(raw).len(),
            4,
            "a lock pass and a recount per chunk"
        );
    }

    /// The bounded repair covers exactly the paths a pending anchor gates on:
    /// its own `.drv` row, and the output rows of the derivations it depends on.
    /// Widening it is PR 3's business; narrowing it silently stops repairing a
    /// path some dispatch gate still reads.
    #[test]
    fn gating_paths_cover_pending_drvs_and_their_dependencies_outputs() {
        let sql = norm(&gating_paths());
        let pending = format!(
            "db.status IN ({})",
            crate::status_sql::build_in(&[
                gradient_entity::build::BuildStatus::Created,
                gradient_entity::build::BuildStatus::Queued,
            ])
        );
        assert_eq!(sql.matches(&pending).count(), 2, "{sql}");
        assert!(
            sql.contains(
                "SELECT d.hash FROM derivation d JOIN derivation_build db ON db.derivation = d.id"
            ),
            "the anchor's own .drv row: {sql}"
        );
        assert!(
            sql.contains("JOIN derivation_dependency e ON e.dependency = o.derivation"),
            "the outputs of its direct dependencies: {sql}"
        );
    }

    /// Nothing to repair issues no statement, so the consistency sweep costs one
    /// query on a graph with no pending anchors.
    #[tokio::test]
    async fn repairing_no_hashes_issues_no_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres).into_connection();
        assert_eq!(repair_counters_for(&db, &[]).await.unwrap(), 0);
        assert!(crate::pool::statements(db.into_transaction_log()).is_empty());
    }
}
