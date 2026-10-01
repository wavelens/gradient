/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::predicates::{builder_predicate, open_predicate};
use crate::graph::promotion::returned_transitions;
use crate::status::TransitionChange;

use super::lock::{ids, lock_shared_builds};
use super::queue::{promote, unpromote_ungated};
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};
use std::sync::LazyLock;

crate::sql! {
    /// A `Created` shared build nothing needs and no entry point names: a build-time
    /// dependency of something we pass through, which will never be built and is not
    /// waiting for anything either. `Skipped` is what settles it on the board.
    ///
    /// Bounded by the ids the need move reports, so the sweep's table-wide form
    /// below is the only pass that reads the whole table.
    SKIP_UNWANTED = "UPDATE derivation_build db SET status = 10, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.derivation = ANY($1::uuid[]) AND db.status = 0 AND NOT db.wanted \
           AND NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation) \
         RETURNING db.derivation, 0 AS from_status, 10 AS to_status",
        params = [DerivationIds(64)];

    /// The mirror, for `Skipped` and `Aborted` alike: the need came back, so the
    /// shared build is pending work again. It goes to `Created` and not to `Queued`, the
    /// promote that follows reads the gates, and an abort's attempts are no verdict.
    THAW_WANTED = "UPDATE derivation_build db SET status = 0, attempt = 0, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.derivation = ANY($1::uuid[]) AND db.status IN (5, 10) AND db.wanted \
         RETURNING db.derivation, old.status AS from_status, 0 AS to_status",
        params = [DerivationIds(64)];

    /// [`SKIP_UNWANTED`] over the whole table: the sweep's backstop for a lost
    /// move, and the backfill for every shared build that was already settled when the
    /// status existed.
    SKIP_UNWANTED_ALL = "UPDATE derivation_build db SET status = 10, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.status = 0 AND NOT db.wanted \
           AND NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation) \
         RETURNING db.derivation, 0 AS from_status, 10 AS to_status",
        params = [],
        tier = Sweep;

    THAW_WANTED_ALL = "UPDATE derivation_build db SET status = 0, attempt = 0, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.status IN (5, 10) AND db.wanted \
         RETURNING db.derivation, old.status AS from_status, 0 AS to_status",
        params = [],
        tier = Sweep;
}

/// Settle every `Created` shared build among `candidates` that nothing needs.
async fn skip_unwanted<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    Ok(returned_transitions(
        db.query_all_raw(SKIP_UNWANTED.bind([ids(candidates)]))
            .await?,
    ))
}

/// Wake every `Skipped` or `Aborted` shared build among `candidates` that something
/// wants again.
async fn thaw_wanted<C: ConnectionTrait>(
    db: &C,
    candidates: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    Ok(returned_transitions(
        db.query_all_raw(THAW_WANTED.bind([ids(candidates)]))
            .await?,
    ))
}

/// The sweep's table-wide pair, run after the need recount so both read a
/// corrected column. Returns every row either moved.
pub async fn settle_skipped<C: ConnectionTrait>(db: &C) -> Result<Vec<TransitionChange>, DbErr> {
    let mut changes = returned_transitions(db.query_all_raw(THAW_WANTED_ALL.stmt()).await?);
    changes.extend(returned_transitions(
        db.query_all_raw(SKIP_UNWANTED_ALL.stmt()).await?,
    ));

    Ok(changes)
}

/// Table-wide, seeded from every entry point: the backstop for a lost update and
/// the backfill the migration deliberately does not carry.
///
/// Absolute rather than incremental for the reason [`seed_blocking_deps`] is: an
/// adjustment cannot express "this shared build keeps its need through a different
/// parent", and a shared build that keeps it re-wants everything below it, which the
/// walk's downward step computes for free. It returns each row it changed WITH its
/// new value, so one statement serves a gain and a loss and no caller has to know
/// which it caused.
///
/// Every open shared build is rewritten and nothing else. A settled shared build keeps whatever
/// it carried, nothing reads it there, and the event that opens it again updates
/// it as a root. A `Completed` shared build whose closure has a missing dependency is open, and its
/// value is what the bounded update below it reads to seed the missing dependency. The scope is
/// the whole open table by design, so the scan the planner answers it with is the
/// right plan and the tier says so.
pub(crate) static RECOUNT_WANTED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "WITH RECURSIVE {cte} \
         UPDATE derivation_build db \
         SET wanted = (db.derivation IN (SELECT derivation FROM wanted)), \
             updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE {open} \
           AND db.wanted <> (db.derivation IN (SELECT derivation FROM wanted)) \
         RETURNING db.derivation, db.wanted",
        cte = crate::graph::walks::open_closure_cte_body("wanted", &open_entry_points(), None),
        open = open_predicate("db"),
    )
});

/// The roots of the need flag: every open shared build an entry point of a retained evaluation
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
    RECOUNT_WANTED = || RECOUNT_WANTED_SQL.as_str(),
        params = [],
        tier = Sweep,
        flags = [Walk];
}

/// Rewrite every shared build whose need flag drifted. Returns how many disagreed, which is
/// the sweep's `need_drift`: a healthy fleet reports zero, and a number that keeps
/// coming back names a mover that is not recomputing what it changed.
pub async fn recount_wanted<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db).await?;
    let rows = walk.query_all_raw(RECOUNT_WANTED.stmt()).await?;
    walk.commit().await?;

    Ok(rows.len() as u64)
}

/// What a bounded update moved: the shared builds that gained need and the ones that
/// lost it, which [`settle_need`] thaws and promotes, or releases and skips.
#[derive(Debug, Default)]
pub struct NeedMoved {
    pub gained: Vec<DerivationId>,
    pub lost: Vec<DerivationId>,
}

static UPDATE_NEED_SQL: LazyLock<String> = LazyLock::new(|| {
    // The roots enter the region as builders so the walk steps out of them once even
    // where they have just stopped being one; everything below is stepped through
    // only while it is.
    let region = crate::graph::walks::open_closure_cte_body(
        "region",
        "SELECT NULL::uuid AS evaluation, unnest($1::uuid[]) AS derivation, true AS builder",
        None,
    );
    // The planner sizes the region from the root count, far past its real size, and
    // a membership subplan it will not hash scans the whole region per probe. Every
    // region test below is a join over whole sets instead; the parent lookup stays a
    // fenced index probe per member. It needs what the walk's own step would: an
    // open, wanted parent outside the region, over a runtime dependency from anything
    // and over any edge from a builder.
    let entered = format!(
        "entered(derivation) AS (SELECT pe.dependency FROM region r, \
         LATERAL (SELECT e.derivation AS parent, e.dependency, e.kind \
                  FROM derivation_dependency e WHERE e.dependency = r.derivation OFFSET 0) pe, \
         LATERAL (SELECT 1 FROM derivation_build p \
                  JOIN derivation pw ON pw.id = p.derivation \
                  WHERE p.derivation = pe.parent AND p.wanted AND {open} \
                    AND (({builder}) OR pe.kind IN (1, 2)) OFFSET 0) q \
         WHERE NOT EXISTS (SELECT 1 FROM region x WHERE x.derivation = pe.parent))",
        open = open_predicate("p"),
        builder = builder_predicate("p", "pw"),
    );
    let seed = format!(
        "SELECT NULL::uuid, r.derivation, ({builder}) FROM region r \
         JOIN derivation_build rb ON rb.derivation = r.derivation \
         JOIN derivation w ON w.id = rb.derivation \
         WHERE {open} \
           AND r.derivation IN (SELECT ep.derivation FROM entry_point ep \
                                UNION ALL SELECT derivation FROM entered)",
        builder = builder_predicate("rb", "w"),
        open = open_predicate("rb"),
    );
    let wanted = crate::graph::walks::open_closure_cte_body("wanted", &seed, Some("region"));

    format!(
        "WITH RECURSIVE {region}, {entered}, {wanted} \
         SELECT DISTINCT r.derivation, (d.derivation IS NOT NULL) AS wanted \
         FROM region r LEFT JOIN (SELECT DISTINCT derivation FROM wanted) d \
           ON d.derivation = r.derivation ORDER BY r.derivation",
    )
});

crate::sql_lazy! {
    UPDATE_NEED = || UPDATE_NEED_SQL.as_str(),
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

crate::sql! {
    /// Apply what [`UPDATE_NEED`] read, as values rather than as a membership
    /// test. `WHERE db.derivation IN (SELECT derivation FROM region)` is a predicate
    /// the planner may answer by reading every shared build and filtering, and it does: a
    /// recursive CTE carries no row estimate worth believing, so a region of a few
    /// dozen loses to a sequential scan of the whole table. A bound array estimates
    /// small, drives a nested loop over the unique index, and takes its row locks in
    /// the derivation order the walk sorted them into.
    WRITE_NEED = r#"
UPDATE derivation_build db
SET wanted = x.wanted, updated_at = (now() AT TIME ZONE 'UTC')
FROM unnest($1::uuid[], $2::bool[]) AS x(derivation, wanted)
WHERE db.derivation = x.derivation AND db.wanted <> x.wanted
RETURNING db.derivation, db.wanted
"#,
        params = [DerivationIds(64), Bools(false, 64)];
}

/// Write the region's updated need flag and return the rows that disagreed, which is
/// what [`update_need`] reports as gained and lost. An empty region writes
/// nothing rather than binding two empty arrays.
async fn write_need(
    txn: &DatabaseTransaction,
    region: &[QueryResult],
) -> Result<Vec<QueryResult>, DbErr> {
    let mut derivations: Vec<uuid::Uuid> = Vec::with_capacity(region.len());
    let mut wanted: Vec<bool> = Vec::with_capacity(region.len());
    for row in region {
        let (Ok(derivation), Ok(want)) = (
            row.try_get::<uuid::Uuid>("", "derivation"),
            row.try_get::<bool>("", "wanted"),
        ) else {
            continue;
        };

        derivations.push(derivation);
        wanted.push(want);
    }

    if derivations.is_empty() {
        return Ok(Vec::new());
    }

    txn.query_all_raw(WRITE_NEED.bind([derivations.into(), wanted.into()]))
        .await
}

/// Update the need flag over `roots` and the pending closure below them, after an event
/// that changed whether they carry it.
///
/// The region includes the roots: a thaw makes a shared build a builder again and its own
/// stored value is as stale as its subtree's. Starts under [`lock_shared_builds`] on the
/// roots; two updates over overlapping regions can still interleave, and the
/// sweep's table-wide recount is the backstop that notices.
///
/// Two statements in one transaction: [`UPDATE_NEED`] walks and answers, and
/// [`WRITE_NEED`] writes the answer it was handed. Naming the region inside the
/// write instead costs a sequential scan of every shared build, for the reason written on
/// that statement. The split widens the window between reading the graph and writing
/// what it implied to a statement boundary; only the roots are locked either way, so
/// the backstop is the same one, and the write still skips a row that already agrees.
pub(crate) async fn update_need<C>(db: &C, roots: &[DerivationId]) -> Result<NeedMoved, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if roots.is_empty() {
        return Ok(NeedMoved::default());
    }

    let txn = crate::graph::walks::begin_walk(db).await?;
    let _lock = lock_shared_builds(&txn, roots).await?;
    let region = txn.query_all_raw(UPDATE_NEED.bind([ids(roots)])).await?;
    let rows = write_need(&txn, &region).await?;
    txn.commit().await?;

    let mut moved = NeedMoved::default();
    for row in &rows {
        let (Ok(derivation), Ok(wanted)) = (
            row.try_get::<uuid::Uuid>("", "derivation"),
            row.try_get::<bool>("", "wanted"),
        ) else {
            continue;
        };

        if wanted {
            moved.gained.push(DerivationId::new(derivation));
        } else {
            moved.lost.push(DerivationId::new(derivation));
        }
    }

    Ok(moved)
}

/// Settle the queue against what a [`update_need`] moved: thaw and queue what
/// gained need, release and skip what lost it.
///
/// Order is load-bearing on both sides. A thaw has to precede the promote or the
/// gate reads a `Skipped` row and passes it over; the skip has to follow the
/// un-promote or it would try to settle a row still `Queued`. One owner, because a
/// caller that got the order wrong would leave a shared build `Skipped` that something
/// had started wanting again, and nothing else ever looks at it.
pub(crate) async fn settle_need<C: ConnectionTrait>(
    db: &C,
    moved: &NeedMoved,
) -> Result<Vec<TransitionChange>, DbErr> {
    let mut changes = Vec::new();
    for gained in moved.gained.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(thaw_wanted(db, gained).await?);
        changes.extend(promote(db, gained).await?);
    }
    for lost in moved.lost.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(unpromote_ungated(db, lost).await?);
        changes.extend(skip_unwanted(db, lost).await?);
    }

    Ok(changes)
}

/// What [`update_and_settle_need`] moved, and the queue transitions it settled
/// the move with.
#[derive(Debug, Default)]
pub struct SettledNeed {
    pub moved: NeedMoved,
    pub changes: Vec<TransitionChange>,
}

/// Update the need flag below `roots` and settle the queue against what moved. The one
/// way another crate moves the need flag, so no event can promote what gained it while
/// leaving a `Skipped` or `Aborted` shared build frozen until the sweep.
pub async fn update_and_settle_need<C>(db: &C, roots: &[DerivationId]) -> Result<SettledNeed, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let moved = update_need(db, roots).await?;
    let changes = settle_need(db, &moved).await?;

    Ok(SettledNeed { moved, changes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::can_start::test_rows::{drv, exec, norm, transition_row};
    use crate::pool::statements;
    use gradient_entity::build::BuildStatus;
    use sea_orm::{DatabaseBackend, MockDatabase, Value};
    use std::collections::BTreeMap;

    fn need_row(id: DerivationId, wanted: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("wanted".to_owned(), Value::from(wanted)),
        ])
    }

    /// `Skipped` is the projection of "Created, and nothing wants it". An entry
    /// point is wanted by definition, so a named root can never be settled by it,
    /// and the thaw goes back to `Created` rather than to the queue: the promote
    /// that follows is what reads the gates.
    #[test]
    fn skip_moves_only_a_created_unwanted_shared_build_no_entry_point_names() {
        let sql = SKIP_UNWANTED.text();
        assert!(sql.contains("SET status = 10"), "{sql}");
        assert!(sql.contains("AND db.status = 0 AND NOT db.wanted"), "{sql}");
        assert!(
            sql.contains(
                "NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation)"
            ),
            "{sql}"
        );

        let thaw = THAW_WANTED.text();
        assert!(
            thaw.contains("SET status = 0, attempt = 0")
                && thaw.contains("AND db.status IN (5, 10) AND db.wanted")
                && thaw.contains("old.status AS from_status"),
            "an aborted shared build is thawed by the same want, with its attempts forgiven: {thaw}"
        );
    }

    /// The update is absolute and writes only the rows that disagree, so its
    /// row count IS the drift and a healthy fleet writes nothing. It returns the
    /// new value per row, because one statement serves both directions: what it
    /// turned on is promoted and what it turned off is un-promoted.
    #[test]
    fn the_need_update_writes_only_disagreeing_rows() {
        let sql = norm(RECOUNT_WANTED_SQL.as_str());
        assert!(
            sql.contains("WITH RECURSIVE wanted(evaluation, derivation, builder) AS"),
            "{sql}"
        );
        assert!(
            sql.contains("SET wanted = (db.derivation IN (SELECT derivation FROM wanted))"),
            "{sql}"
        );
        assert!(
            sql.contains("db.wanted <> (db.derivation IN (SELECT derivation FROM wanted))"),
            "rewriting agreeing rows reports drift that is not there: {sql}"
        );
        assert!(
            sql.contains("RETURNING db.derivation, db.wanted"),
            "the caller settles the queue from the new value: {sql}"
        );
    }

    /// The backstop is the last writer that can un-strand a subtree, so it writes
    /// every open shared build: a `Skipped` one, which need returning thaws, and a
    /// `Completed` one with a missing dependency in its closure, whose value seeds the bounded
    /// update below it. Restricted to a status list it read the walk's correct
    /// answer and then declined to apply it, twice over.
    #[test]
    fn the_backstop_rewrites_every_open_shared_build() {
        let sql = norm(RECOUNT_WANTED_SQL.as_str());
        assert!(
            sql.contains(&format!(
                "WHERE {} AND db.wanted <>",
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

    /// Every event that moves need settles the queue against it: a shared build that
    /// gained need while `Skipped` or `Aborted` is thawed before the promote reads
    /// its gate, and one that lost it is released and skipped, instead of both
    /// waiting for the consistency check's table-wide pair.
    #[tokio::test]
    async fn a_need_move_thaws_what_gained_it_and_skips_what_lost_it() {
        let root = DerivationId::now_v7();
        let on = DerivationId::now_v7();
        let off = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0), exec(1)])
            .append_query_results([vec![need_row(on, true), need_row(off, false)]])
            .append_query_results([vec![need_row(on, true), need_row(off, false)]])
            .append_query_results([vec![transition_row(on, 10, 0)], vec![drv(on)]])
            .append_query_results([empty, vec![transition_row(off, 0, 10)]])
            .into_connection();

        let settled = update_and_settle_need(&db, &[root]).await.unwrap();

        assert_eq!(settled.moved.gained, vec![on]);
        assert_eq!(settled.moved.lost, vec![off]);
        let moves: Vec<_> = settled
            .changes
            .iter()
            .map(|c| (c.derivation, c.from, c.to))
            .collect();
        assert_eq!(
            moves,
            vec![
                (on, BuildStatus::Skipped, BuildStatus::Created),
                (on, BuildStatus::Created, BuildStatus::Queued),
                (off, BuildStatus::Created, BuildStatus::Skipped),
            ]
        );
        let log = statements(db.into_transaction_log());
        let thaw = log
            .iter()
            .position(|s| s.contains("db.status IN (5, 10) AND db.wanted"))
            .expect("what became wanted is thawed");
        let promote = log
            .iter()
            .position(|s| s.contains("queued_at = coalesce(db.queued_at"))
            .expect("and then promoted");
        assert!(thaw < promote, "{log:?}");
    }

    /// Both directions, over a region that INCLUDES the roots. A thawed shared build's own
    /// need is as stale as anything below it - its value was last written when it
    /// was terminal - so an update that only walked downward would pass busybox through
    /// again the moment a retire reset it (#666). The seed comes from outside the
    /// region, because a member kept by an outside parent re-wants its own
    /// subtree. The walk answers and the write is handed what it answered.
    #[tokio::test]
    async fn the_bounded_update_covers_its_roots_and_seeds_from_outside() {
        let root = DerivationId::now_v7();
        let on = DerivationId::now_v7();
        let off = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0), exec(1)])
            .append_query_results([vec![need_row(on, true), need_row(off, false)]])
            .append_query_results([vec![need_row(on, true), need_row(off, false)]])
            .into_connection();

        let moved = update_need(&db, &[root]).await.unwrap();
        assert_eq!(moved.gained, vec![on]);
        assert_eq!(moved.lost, vec![off]);

        let log = statements(db.into_transaction_log());
        assert!(
            log[0].contains("SET LOCAL work_mem")
                && log[1].contains("ORDER BY derivation FOR NO KEY UPDATE"),
            "the walk its plan gate measures is raised and its roots locked: {log:?}"
        );
        let walk = norm(&log[2]);
        assert!(
            walk.contains("region(evaluation, derivation, builder) AS"),
            "{walk}"
        );
        assert!(
            walk.contains(
                "WHERE NOT EXISTS (SELECT 1 FROM region x WHERE x.derivation = pe.parent))"
            ),
            "the seed must come from wanters OUTSIDE the region: {walk}"
        );
        // The planner sizes the region from the root count, and an estimate past
        // hash_mem turns every `IN (SELECT .. FROM region)` into a scan of the whole
        // CTE per probe: 6.9k roots ran 41 min. Every region test is therefore a
        // join over whole sets, never a subplan, and the wanted step's test sits
        // outside its fenced probe so it executes once per level, not once per row.
        assert!(
            walk.contains(
                "OFFSET 0) s OFFSET 0) t WHERE EXISTS (SELECT 1 FROM region x WHERE x.derivation = t.next))"
            ),
            "the wanted step semi-joins the region once per level: {walk}"
        );
        assert!(
            !walk.contains("IN (SELECT derivation FROM region)")
                && !walk.contains("IN (SELECT derivation FROM wanted)"),
            "no region or wanted membership is a subplan: {walk}"
        );
        assert!(
            !walk.contains("JOIN derivation_build p ON"),
            "the parent is looked up by its key, not joined: {walk}"
        );
        // The seed is a second expression of the walk's own step, so it stops
        // where the walk stops and nowhere else: an open, wanted parent, over a
        // runtime dependency from anything and over any edge from a builder. A passthrough's
        // runtime references are wanted here too, and so are the references of
        // a `Completed` shared build whose closure has a missing dependency.
        assert!(
            walk.contains(
                "entered(derivation) AS (SELECT pe.dependency FROM region r, \
                 LATERAL (SELECT e.derivation AS parent, e.dependency, e.kind \
                 FROM derivation_dependency e WHERE e.dependency = r.derivation OFFSET 0) pe, \
                 LATERAL (SELECT 1 FROM derivation_build p \
                 JOIN derivation pw ON pw.id = p.derivation \
                 WHERE p.derivation = pe.parent AND p.wanted \
                 AND (NOT p.fetchable AND p.status NOT IN (4, 6, 9)) \
                 AND ((pw.walked AND p.probed AND NOT p.cache_available \
                 AND p.status IN (0, 1, 2, 8)) OR pe.kind IN (1, 2)) OFFSET 0) q"
            ),
            "{walk}"
        );
        assert!(
            walk.contains(
                "FROM region r JOIN derivation_build rb ON rb.derivation = r.derivation \
                 JOIN derivation w ON w.id = rb.derivation \
                 WHERE (NOT rb.fetchable AND rb.status NOT IN (4, 6, 9)) \
                 AND r.derivation IN (SELECT ep.derivation FROM entry_point ep \
                 UNION ALL SELECT derivation FROM entered)"
            ),
            "a settled root seeds nothing, and a seed carries its own builder bit: {walk}"
        );
        assert!(
            !walk.contains("build_job"),
            "a name is what adoption writes for what the walk reaches: {walk}"
        );
        assert!(
            walk.contains(
                "FROM region r LEFT JOIN (SELECT DISTINCT derivation FROM wanted) d \
                 ON d.derivation = r.derivation ORDER BY r.derivation"
            ),
            "the write takes its locks in the order the walk sorted: {walk}"
        );
        let write = norm(&log[3]);
        assert!(
            write.contains("FROM unnest($1::uuid[], $2::bool[]) AS x(derivation, wanted)")
                && write.contains("RETURNING db.derivation, db.wanted"),
            "the region reaches the write as values, not as a subquery: {write}"
        );
    }
}
