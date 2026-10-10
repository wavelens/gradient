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
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};
use std::sync::LazyLock;

crate::sql! {
    SKIP_UNWANTED = "UPDATE derivation_build db SET status = 10, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.derivation = ANY($1::uuid[]) AND db.status = 0 AND NOT db.wanted \
           AND NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation) \
         RETURNING db.derivation, 0 AS from_status, 10 AS to_status",
        params = [DerivationIds(64)];

    THAW_WANTED = "UPDATE derivation_build db SET status = 0, attempt = 0, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.derivation = ANY($1::uuid[]) AND db.status IN (5, 10) AND db.wanted \
         RETURNING db.derivation, old.status AS from_status, 0 AS to_status",
        params = [DerivationIds(64)];

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

pub async fn settle_skipped<C: ConnectionTrait>(db: &C) -> Result<Vec<TransitionChange>, DbErr> {
    let mut changes = returned_transitions(db.query_all_raw(THAW_WANTED_ALL.stmt()).await?);
    changes.extend(returned_transitions(
        db.query_all_raw(SKIP_UNWANTED_ALL.stmt()).await?,
    ));

    Ok(changes)
}

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

/// A finished evaluation and an aborted entry point want nothing. Aborted shared builds are
/// open, and such entry points kept them wanted and thawed.
fn wanted_by_its_evaluation(entry_point: &str) -> String {
    format!(
        "JOIN evaluation ev ON ev.id = {entry_point}.evaluation AND ev.status NOT IN ({finished}) \
         AND NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.evaluation = {entry_point}.evaluation \
                         AND bj.derivation = {entry_point}.derivation AND bj.aborted)",
        finished = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
    )
}

fn open_entry_points() -> String {
    format!(
        "SELECT NULL::uuid, db.derivation, ({builder}) FROM entry_point ep {live} \
         JOIN derivation_build db ON db.derivation = ep.derivation \
         JOIN derivation w ON w.id = db.derivation WHERE {open}",
        live = wanted_by_its_evaluation("ep"),
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

pub async fn recount_wanted<C>(db: &C) -> Result<u64, DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let walk = crate::graph::walks::begin_walk(db, &RECOUNT_WANTED).await?;
    let rows = walk.query_all_raw(RECOUNT_WANTED.stmt()).await?;
    walk.commit().await?;

    Ok(rows.len() as u64)
}

#[derive(Debug, Default)]
pub struct NeedMoved {
    pub gained: Vec<DerivationId>,
    pub lost: Vec<DerivationId>,
}

static UPDATE_NEED_SQL: LazyLock<String> = LazyLock::new(|| {
    let region = crate::graph::walks::open_closure_cte_body(
        "region",
        "SELECT NULL::uuid AS evaluation, unnest($1::uuid[]) AS derivation, true AS builder",
        None,
    );
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
           AND r.derivation IN (SELECT ep.derivation FROM entry_point ep {live} \
                                UNION ALL SELECT derivation FROM entered)",
        live = wanted_by_its_evaluation("ep"),
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
        flags = [Walk, DefaultWorkMem];
}

crate::sql! {
    /// The region is applied as bound arrays, not as a membership test.
    /// A recursive CTE is carrying no usable row estimate.
    /// The planner would answer a membership test with a scan of every shared build.
    /// A bound array is estimating small and is driving a nested loop over the unique index.
    WRITE_NEED = r#"
UPDATE derivation_build db
SET wanted = x.wanted, updated_at = (now() AT TIME ZONE 'UTC')
FROM unnest($1::uuid[], $2::bool[]) AS x(derivation, wanted)
WHERE db.derivation = x.derivation AND db.wanted <> x.wanted
RETURNING db.derivation, db.wanted
"#,
        params = [DerivationIds(64), Bools(false, 64)];
}

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

pub(crate) async fn update_need<C>(db: &C, roots: &[DerivationId]) -> Result<NeedMoved, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if roots.is_empty() {
        return Ok(NeedMoved::default());
    }

    let txn = crate::graph::walks::begin_walk(db, &UPDATE_NEED).await?;
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

/// Order is load-bearing on both sides.
/// A thaw must precede the promote, or the gate would pass over a `Skipped` row.
/// The skip must follow the un-promote, or it would try to settle a row still `Queued`.
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

#[derive(Debug, Default)]
pub struct SettledNeed {
    pub moved: NeedMoved,
    pub changes: Vec<TransitionChange>,
}

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

    #[test]
    fn an_entry_point_of_a_finished_evaluation_wants_nothing() {
        let live = "FROM entry_point ep JOIN evaluation ev \
                    ON ev.id = ep.evaluation AND ev.status NOT IN (5, 6, 7)";
        for sql in [RECOUNT_WANTED_SQL.as_str(), UPDATE_NEED_SQL.as_str()] {
            let sql = norm(sql);
            assert_eq!(
                sql.matches("FROM entry_point ep").count(),
                sql.matches(live).count(),
                "an aborted evaluation must not keep its own builds wanted: {sql}"
            );
            assert!(sql.contains(live), "{sql}");
            assert!(
                sql.contains(
                    "AND NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.evaluation = ep.evaluation \
                     AND bj.derivation = ep.derivation AND bj.aborted)"
                ),
                "an entry point its evaluation aborted must not thaw: {sql}"
            );
        }
    }

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
                 JOIN evaluation ev ON ev.id = ep.evaluation AND ev.status NOT IN (5, 6, 7) \
                 AND NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.evaluation = ep.evaluation \
                 AND bj.derivation = ep.derivation AND bj.aborted) \
                 UNION ALL SELECT derivation FROM entered)"
            ),
            "a settled root seeds nothing, and a seed carries its own builder bit: {walk}"
        );
        assert_eq!(
            walk.matches("build_job").count(),
            1,
            "a name is what adoption writes for what the walk reaches, and the walk reads \
             only the abort of an entry point from it: {walk}"
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
