/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-entry-point dependency histograms for the task page. A page's stale
//! entry points are recomputed by the fenced walk in
//! [`crate::task_board::entry_point_dep_counts`] and cached in
//! `entry_point_dep_count` under the evaluation's `graph_version`, which every
//! anchor move, every ingest batch and the startup recovery bump.

use crate::fetch_in_chunks;
use gradient_entity::build::BuildStatus;
use gradient_entity::ids::{DerivationId, EntryPointId, EvaluationId};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, DbErr, EntityTrait, QueryFilter,
    Statement, TransactionTrait,
};
use std::collections::HashMap;

pub type DepCounts = HashMap<EntryPointId, HashMap<BuildStatus, i64>>;

/// Advance the graph version of `evaluations`. The rows are locked in id order
/// so two emits over overlapping evaluation sets cannot deadlock on them.
pub async fn bump_graph_version<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<u64, DbErr> {
    if evaluations.is_empty() {
        return Ok(0);
    }

    let ids: Vec<uuid::Uuid> = evaluations.iter().map(|e| e.into_inner()).collect();
    let res = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE evaluation e SET graph_version = e.graph_version + 1 \
             FROM (SELECT id FROM evaluation WHERE id = ANY($1::uuid[]) \
                   ORDER BY id FOR UPDATE) locked \
             WHERE e.id = locked.id",
            [ids.into()],
        ))
        .await?;

    Ok(res.rows_affected())
}

/// The same bump for every evaluation with a `build_job` on one of `derivations`,
/// for a mover that has no `DbContext` and so no emitter (startup recovery).
pub async fn bump_graph_version_for_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<u64, DbErr> {
    if derivations.is_empty() {
        return Ok(0);
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    let res = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE evaluation e SET graph_version = e.graph_version + 1 \
             FROM (SELECT id FROM evaluation \
                   WHERE id IN (SELECT evaluation FROM build_job WHERE derivation = ANY($1::uuid[])) \
                   ORDER BY id FOR UPDATE) locked \
             WHERE e.id = locked.id",
            [ids.into()],
        ))
        .await?;

    Ok(res.rows_affected())
}

/// The histogram of every entry point on a page: read from the cache where the
/// stamp equals the evaluation's current version, recomputed by one fenced walk
/// and stored under that version otherwise. An entry point with no dependencies
/// is absent from the map.
pub async fn cached_entry_point_dep_counts<C>(
    db: &C,
    evaluation: &MEvaluation,
    entry_points: &[MEntryPoint],
) -> Result<DepCounts, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let version = evaluation.graph_version;
    let (fresh, stale): (Vec<&MEntryPoint>, Vec<&MEntryPoint>) = entry_points
        .iter()
        .partition(|ep| ep.dep_counts_version == Some(version));

    let fresh_ids: Vec<EntryPointId> = fresh.iter().map(|ep| ep.id).collect();
    let mut out = if fresh_ids.is_empty() {
        DepCounts::new()
    } else {
        load_entry_point_dep_counts(db, &fresh_ids).await?
    };

    if stale.is_empty() {
        return Ok(out);
    }

    let seeds: Vec<(EntryPointId, uuid::Uuid)> = stale
        .iter()
        .map(|ep| (ep.id, ep.derivation.into_inner()))
        .collect();
    let computed = crate::task_board::entry_point_dep_counts(db, evaluation.id, &seeds).await?;
    let stale_ids: Vec<EntryPointId> = stale.iter().map(|ep| ep.id).collect();
    store_entry_point_dep_counts(db, version, &stale_ids, &computed).await?;
    out.extend(computed);

    Ok(out)
}

async fn store_entry_point_dep_counts<C>(
    db: &C,
    version: i64,
    entry_points: &[EntryPointId],
    counts: &DepCounts,
) -> Result<(), DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    let ids: Vec<uuid::Uuid> = entry_points.iter().map(|ep| ep.into_inner()).collect();
    let mut eps: Vec<uuid::Uuid> = Vec::new();
    let mut statuses: Vec<i32> = Vec::new();
    let mut cnts: Vec<i64> = Vec::new();
    for ep in entry_points {
        for (status, n) in counts.get(ep).into_iter().flatten() {
            eps.push(ep.into_inner());
            statuses.push(i32::from(*status));
            cnts.push(*n);
        }
    }

    let txn = db.begin().await?;
    txn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "DELETE FROM entry_point_dep_count WHERE entry_point = ANY($1::uuid[])",
        [ids.clone().into()],
    ))
    .await?;

    if !eps.is_empty() {
        txn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO entry_point_dep_count (id, entry_point, status, count) \
             SELECT uuidv7(), r.entry_point, r.status, r.count \
             FROM unnest($1::uuid[], $2::int[], $3::bigint[]) AS r(entry_point, status, count) \
             ON CONFLICT (entry_point, status) DO UPDATE SET count = EXCLUDED.count",
            [eps.into(), statuses.into(), cnts.into()],
        ))
        .await?;
    }

    txn.execute_raw(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE entry_point SET dep_counts_version = $1 WHERE id = ANY($2::uuid[])",
        [version.into(), ids.into()],
    ))
    .await?;

    txn.commit().await
}

/// Load the cached `entry_point -> status -> count` rows of `entry_points`.
pub async fn load_entry_point_dep_counts<C: ConnectionTrait>(
    db: &C,
    entry_points: &[EntryPointId],
) -> Result<DepCounts, DbErr> {
    let rows = fetch_in_chunks(entry_points, |chunk| async move {
        EEntryPointDepCount::find()
            .filter(CEntryPointDepCount::EntryPoint.is_in(chunk))
            .all(db)
            .await
    })
    .await?;

    let mut out = DepCounts::new();
    for r in rows {
        if let Ok(status) = BuildStatus::try_from(r.status) {
            *out.entry(r.entry_point)
                .or_default()
                .entry(status)
                .or_insert(0) += r.count;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::ids::EntryPointDepCountId;
    use sea_orm::{DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn ok(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    fn count_row(
        entry_point: EntryPointId,
        status: BuildStatus,
        count: i64,
    ) -> MEntryPointDepCount {
        MEntryPointDepCount {
            id: EntryPointDepCountId::now_v7(),
            entry_point,
            status: i32::from(status),
            count,
        }
    }

    fn walk_row(
        entry_point: EntryPointId,
        status: BuildStatus,
        cnt: i64,
    ) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "entry_point".to_owned(),
                Value::from(entry_point.into_inner()),
            ),
            ("status".to_owned(), Value::from(i32::from(status))),
            ("cnt".to_owned(), Value::from(cnt)),
        ])
    }

    fn entry_point(evaluation: EvaluationId, stamp: Option<i64>) -> MEntryPoint {
        MEntryPoint {
            id: EntryPointId::now_v7(),
            evaluation,
            derivation: DerivationId::now_v7(),
            dep_counts_version: stamp,
            ..Default::default()
        }
    }

    fn evaluation(graph_version: i64) -> MEvaluation {
        MEvaluation {
            id: EvaluationId::now_v7(),
            graph_version,
            ..Default::default()
        }
    }

    fn sqls(db: DatabaseConnection) -> Vec<String> {
        db.into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().iter().map(|s| s.sql.clone()))
            .collect()
    }

    /// The fresh entry point is served from its rows and never walked; the stale
    /// one is walked, its rows replaced under the version the walk ran under.
    #[tokio::test]
    async fn a_fresh_stamp_reads_the_cache_and_a_stale_one_walks_then_stores() {
        let eval = evaluation(3);
        let fresh = entry_point(eval.id, Some(3));
        let stale = entry_point(eval.id, Some(2));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(fresh.id, BuildStatus::Completed, 3)]])
            .append_exec_results([ok(0)])
            .append_query_results([vec![walk_row(stale.id, BuildStatus::Building, 1)]])
            .append_exec_results([ok(2), ok(1), ok(1)])
            .into_connection();

        let out = cached_entry_point_dep_counts(&db, &eval, &[fresh.clone(), stale.clone()])
            .await
            .unwrap();

        assert_eq!(out[&fresh.id][&BuildStatus::Completed], 3);
        assert_eq!(out[&stale.id][&BuildStatus::Building], 1);
        let log = sqls(db);
        let walk = log
            .iter()
            .find(|s| s.contains("WITH RECURSIVE seeds"))
            .expect("the stale entry point is walked");

        assert!(!walk.contains("dep_counts_version"), "{walk}");
        let stamp = log
            .iter()
            .position(|s| s.contains("SET dep_counts_version = $1"))
            .expect("the stamp is written");
        let delete = log
            .iter()
            .position(|s| s.starts_with("DELETE FROM entry_point_dep_count"))
            .expect("the old rows go first");
        let insert = log
            .iter()
            .position(|s| s.contains("ON CONFLICT (entry_point, status) DO UPDATE"))
            .expect("the new rows land");

        assert!(delete < insert && insert < stamp, "{log:?}");
    }

    /// A page whose stamps all match issues one read and no write at all.
    #[tokio::test]
    async fn a_fully_fresh_page_never_walks_or_writes() {
        let eval = evaluation(7);
        let ep = entry_point(eval.id, Some(7));
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(ep.id, BuildStatus::Queued, 5)]])
            .into_connection();

        let out = cached_entry_point_dep_counts(&db, &eval, std::slice::from_ref(&ep))
            .await
            .unwrap();

        assert_eq!(out[&ep.id][&BuildStatus::Queued], 5);
        assert_eq!(sqls(db).len(), 1);
    }

    /// A leaf entry point (no dependencies) has no rows to insert but must still
    /// be stamped, or every read would walk it again.
    #[tokio::test]
    async fn a_leaf_is_stamped_without_an_insert() {
        let eval = evaluation(1);
        let leaf = entry_point(eval.id, None);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([ok(0)])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([ok(0), ok(1)])
            .into_connection();

        let out = cached_entry_point_dep_counts(&db, &eval, std::slice::from_ref(&leaf))
            .await
            .unwrap();

        assert!(out.is_empty());
        let log = sqls(db);

        assert!(
            !log.iter()
                .any(|s| s.contains("INSERT INTO entry_point_dep_count")),
            "{log:?}"
        );
        assert!(
            log.iter()
                .any(|s| s.contains("SET dep_counts_version = $1")),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn the_bump_locks_the_evaluations_in_id_order_and_skips_an_empty_set() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([ok(2)])
            .into_connection();

        assert_eq!(bump_graph_version(&db, &[]).await.unwrap(), 0);
        let n = bump_graph_version(&db, &[EvaluationId::now_v7(), EvaluationId::now_v7()])
            .await
            .unwrap();

        assert_eq!(n, 2);
        let log = sqls(db);

        assert_eq!(log.len(), 1, "{log:?}");
        assert!(
            log[0].contains("graph_version = e.graph_version + 1"),
            "{}",
            log[0]
        );
        assert!(log[0].contains("ORDER BY id FOR UPDATE"), "{}", log[0]);
    }
}
