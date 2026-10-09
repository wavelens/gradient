/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::fetch_in_chunks;
use chrono::NaiveDateTime;
use gradient_entity::build::BuildStatus;
use gradient_entity::ids::{DerivationId, EntryPointId, EvaluationId};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, FromQueryResult,
    QueryFilter, TransactionTrait,
};
use std::collections::{HashMap, HashSet};

pub type DepCounts = HashMap<EntryPointId, HashMap<BuildStatus, i64>>;

/// The refresh must stay well above the walk's cost, or the updates would never drain.
/// A 15 s value against a 93 s walk was 80% of the database's time.
pub const DEP_COUNTS_REFRESH_SECS: i64 = 120;

pub const DEP_COUNTS_MAX_AGE_SECS: i64 = 600;

fn needs_update(ep: &MEntryPoint, version: i64, now: NaiveDateTime) -> bool {
    let Some(computed_at) = ep.dep_counts_computed_at else {
        return true;
    };

    let age = now.signed_duration_since(computed_at).num_seconds();
    if age >= DEP_COUNTS_MAX_AGE_SECS {
        return true;
    }

    ep.dep_counts_version != Some(version) && age >= DEP_COUNTS_REFRESH_SECS
}

crate::sql! {
    BUMP_GRAPH_VERSION = "UPDATE evaluation e SET graph_version = e.graph_version + 1 \
             FROM (SELECT id FROM evaluation WHERE id = ANY($1::uuid[]) \
                   ORDER BY id FOR NO KEY UPDATE) locked \
             WHERE e.id = locked.id",
        params = [EvaluationIds(64)];

    BUMP_GRAPH_VERSION_FOR_DERIVATIONS = "UPDATE evaluation e SET graph_version = e.graph_version + 1 \
             FROM (SELECT id FROM evaluation \
                   WHERE id = ANY(ARRAY(SELECT DISTINCT evaluation FROM build_job \
                                        WHERE derivation = ANY($1::uuid[]))) \
                   ORDER BY id FOR NO KEY UPDATE) locked \
             WHERE e.id = locked.id",
        params = [DerivationIds(64)];
}

pub async fn bump_graph_version<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<u64, DbErr> {
    if evaluations.is_empty() {
        return Ok(0);
    }

    let ids: Vec<uuid::Uuid> = evaluations.iter().map(|e| e.into_inner()).collect();
    let res = db
        .execute_raw(BUMP_GRAPH_VERSION.bind([ids.into()]))
        .await?;

    Ok(res.rows_affected())
}

pub async fn bump_graph_version_for_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<u64, DbErr> {
    if derivations.is_empty() {
        return Ok(0);
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    let res = db
        .execute_raw(BUMP_GRAPH_VERSION_FOR_DERIVATIONS.bind([ids.into()]))
        .await?;

    Ok(res.rows_affected())
}

pub async fn cached_entry_point_dep_counts<C>(
    db: &C,
    evaluation: &MEvaluation,
    entry_points: &[MEntryPoint],
) -> Result<DepCounts, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let version = evaluation.graph_version;
    let now = gradient_types::now();
    let stale: Vec<EntryPointId> = entry_points
        .iter()
        .filter(|ep| needs_update(ep, version, now))
        .map(|ep| ep.id)
        .collect();
    let claimed = claim_refresh(db, now, &stale).await?;
    let (walked, stored): (Vec<&MEntryPoint>, Vec<&MEntryPoint>) =
        entry_points.iter().partition(|ep| claimed.contains(&ep.id));

    let stored_ids: Vec<EntryPointId> = stored.iter().map(|ep| ep.id).collect();
    let mut out = if stored_ids.is_empty() {
        DepCounts::new()
    } else {
        load_entry_point_dep_counts(db, &stored_ids).await?
    };

    if walked.is_empty() {
        return Ok(out);
    }

    let seeds: Vec<(EntryPointId, uuid::Uuid)> = walked
        .iter()
        .map(|ep| (ep.id, ep.derivation.into_inner()))
        .collect();
    let computed = crate::task_board::entry_point_dep_counts(db, evaluation.id, &seeds).await?;
    let walked_ids: Vec<EntryPointId> = walked.iter().map(|ep| ep.id).collect();
    store_entry_point_dep_counts(db, version, now, &walked_ids, &computed).await?;
    out.extend(computed);

    Ok(out)
}

#[derive(Debug, FromQueryResult)]
struct ClaimedRow {
    id: uuid::Uuid,
}

crate::sql! {
    CLAIM_DEP_COUNTS_REFRESH = "UPDATE entry_point SET dep_counts_computed_at = $1 \
         WHERE id = ANY($3::uuid[]) \
           AND (dep_counts_computed_at IS NULL OR dep_counts_computed_at <= $2) \
         RETURNING id",
        params = [Now, Now, EntryPointIds(64)];
}

/// A walk that never stores is leaving its claim to expire after the refresh interval.
async fn claim_refresh<C: ConnectionTrait>(
    db: &C,
    now: NaiveDateTime,
    stale: &[EntryPointId],
) -> Result<HashSet<EntryPointId>, DbErr> {
    if stale.is_empty() {
        return Ok(HashSet::new());
    }

    let refreshed_before = now - chrono::Duration::seconds(DEP_COUNTS_REFRESH_SECS);
    let ids: Vec<uuid::Uuid> = stale.iter().map(|ep| ep.into_inner()).collect();
    let rows = ClaimedRow::find_by_statement(CLAIM_DEP_COUNTS_REFRESH.bind([
        now.into(),
        refreshed_before.into(),
        ids.into(),
    ]))
    .all(db)
    .await?;

    Ok(rows.into_iter().map(|row| EntryPointId(row.id)).collect())
}

crate::sql! {
    DELETE_ENTRY_POINT_DEP_COUNTS = "DELETE FROM entry_point_dep_count WHERE entry_point = ANY($1::uuid[])",
        params = [EntryPointIds(64)];

    INSERT_ENTRY_POINT_DEP_COUNTS = "INSERT INTO entry_point_dep_count (id, entry_point, status, count) \
             SELECT uuidv7(), r.entry_point, r.status, r.count \
             FROM unnest($1::uuid[], $2::int[], $3::bigint[]) AS r(entry_point, status, count) \
             ON CONFLICT (entry_point, status) DO UPDATE SET count = EXCLUDED.count",
        params = [EntryPointIds(64), Ints(2, 64), Ints(5, 64)];

    UPDATE_ENTRY_POINT_DEP_COUNTS_STAMP = "UPDATE entry_point SET dep_counts_version = $1, dep_counts_computed_at = $2 \
         WHERE id = ANY($3::uuid[])",
        params = [Int(2), Now, EntryPointIds(64)];
}

async fn store_entry_point_dep_counts<C>(
    db: &C,
    version: i64,
    computed_at: NaiveDateTime,
    entry_points: &[EntryPointId],
    counts: &DepCounts,
) -> Result<(), DbErr>
where
    C: TransactionTrait<Transaction = DatabaseTransaction>,
{
    // Both arrays are sorted to give overlapping concurrent refreshes the same lock order.
    let mut ids: Vec<uuid::Uuid> = entry_points.iter().map(|ep| ep.into_inner()).collect();
    ids.sort_unstable();

    let mut rows: Vec<(uuid::Uuid, i32, i64)> = entry_points
        .iter()
        .flat_map(|ep| {
            counts
                .get(ep)
                .into_iter()
                .flatten()
                .map(|(status, n)| (ep.into_inner(), i32::from(*status), *n))
        })
        .collect();
    rows.sort_unstable();

    let eps: Vec<uuid::Uuid> = rows.iter().map(|r| r.0).collect();
    let statuses: Vec<i32> = rows.iter().map(|r| r.1).collect();
    let cnts: Vec<i64> = rows.iter().map(|r| r.2).collect();

    let txn = db.begin().await?;
    txn.execute_raw(DELETE_ENTRY_POINT_DEP_COUNTS.bind([ids.clone().into()]))
        .await?;

    if !eps.is_empty() {
        txn.execute_raw(INSERT_ENTRY_POINT_DEP_COUNTS.bind([
            eps.into(),
            statuses.into(),
            cnts.into(),
        ]))
        .await?;
    }

    txn.execute_raw(UPDATE_ENTRY_POINT_DEP_COUNTS_STAMP.bind([
        version.into(),
        computed_at.into(),
        ids.into(),
    ]))
    .await?;

    txn.commit().await
}

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

    fn claimed(entry_points: &[EntryPointId]) -> Vec<BTreeMap<String, Value>> {
        entry_points
            .iter()
            .map(|ep| BTreeMap::from([("id".to_owned(), Value::from(ep.into_inner()))]))
            .collect()
    }

    fn entry_point(evaluation: EvaluationId, stamp: Option<i64>, age_secs: i64) -> MEntryPoint {
        MEntryPoint {
            id: EntryPointId::now_v7(),
            evaluation,
            derivation: DerivationId::now_v7(),
            dep_counts_version: stamp,
            dep_counts_computed_at: Some(
                gradient_types::now() - chrono::Duration::seconds(age_secs),
            ),
            ..Default::default()
        }
    }

    fn never_computed(evaluation: EvaluationId) -> MEntryPoint {
        MEntryPoint {
            id: EntryPointId::now_v7(),
            evaluation,
            derivation: DerivationId::now_v7(),
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

    #[tokio::test]
    async fn a_fresh_stamp_reads_the_cache_and_a_stale_one_walks_then_stores() {
        let eval = evaluation(3);
        let fresh = entry_point(eval.id, Some(3), 1);
        let stale = entry_point(eval.id, Some(2), DEP_COUNTS_REFRESH_SECS + 1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([claimed(&[stale.id])])
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

    #[tokio::test]
    async fn a_fully_fresh_page_never_walks_or_writes() {
        let eval = evaluation(7);
        let ep = entry_point(eval.id, Some(7), 1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(ep.id, BuildStatus::Queued, 5)]])
            .into_connection();

        let out = cached_entry_point_dep_counts(&db, &eval, std::slice::from_ref(&ep))
            .await
            .unwrap();

        assert_eq!(out[&ep.id][&BuildStatus::Queued], 5);
        assert_eq!(sqls(db).len(), 1);
    }

    #[tokio::test]
    async fn a_leaf_is_stamped_without_an_insert() {
        let eval = evaluation(1);
        let leaf = never_computed(eval.id);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([claimed(&[leaf.id])])
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
    async fn a_just_computed_page_is_served_even_though_the_graph_moved() {
        let eval = evaluation(99);
        let ep = entry_point(eval.id, Some(4), DEP_COUNTS_REFRESH_SECS - 1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![count_row(ep.id, BuildStatus::Building, 2)]])
            .into_connection();

        let out = cached_entry_point_dep_counts(&db, &eval, std::slice::from_ref(&ep))
            .await
            .unwrap();

        assert_eq!(out[&ep.id][&BuildStatus::Building], 2);
        assert_eq!(sqls(db).len(), 1, "a damped read neither walks nor writes");
    }

    #[tokio::test]
    async fn rows_past_the_age_ceiling_update_even_with_a_matching_stamp() {
        let eval = evaluation(5);
        let ep = entry_point(eval.id, Some(5), DEP_COUNTS_MAX_AGE_SECS + 1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([claimed(&[ep.id])])
            .append_exec_results([ok(0)])
            .append_query_results([vec![walk_row(ep.id, BuildStatus::Completed, 4)]])
            .append_exec_results([ok(1), ok(1), ok(1)])
            .into_connection();

        let out = cached_entry_point_dep_counts(&db, &eval, std::slice::from_ref(&ep))
            .await
            .unwrap();

        assert_eq!(out[&ep.id][&BuildStatus::Completed], 4);
        let log = sqls(db);

        assert!(
            log.iter().any(|s| s.contains("WITH RECURSIVE seeds")),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn a_refresh_claimed_by_a_concurrent_page_load_reads_the_stored_counts() {
        let eval = evaluation(8);
        let ep = entry_point(eval.id, Some(7), DEP_COUNTS_REFRESH_SECS + 1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([claimed(&[])])
            .append_query_results([vec![count_row(ep.id, BuildStatus::Queued, 6)]])
            .into_connection();

        let out = cached_entry_point_dep_counts(&db, &eval, std::slice::from_ref(&ep))
            .await
            .unwrap();

        assert_eq!(out[&ep.id][&BuildStatus::Queued], 6);
        let log = sqls(db);

        assert!(
            log[0].contains("dep_counts_computed_at <= $2"),
            "the claim runs first: {log:?}"
        );
        assert!(
            !log.iter().any(|s| s.contains("WITH RECURSIVE seeds")),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn the_store_binds_its_ids_in_sorted_order() {
        let eval = evaluation(2);
        let a = never_computed(eval.id);
        let b = never_computed(eval.id);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([claimed(&[a.id, b.id])])
            .append_exec_results([ok(0)])
            .append_query_results([vec![
                walk_row(a.id, BuildStatus::Queued, 1),
                walk_row(b.id, BuildStatus::Queued, 1),
            ]])
            .append_exec_results([ok(0), ok(2), ok(2)])
            .into_connection();

        cached_entry_point_dep_counts(&db, &eval, &[a.clone(), b.clone()])
            .await
            .unwrap();

        let mut expected = [a.id.into_inner(), b.id.into_inner()];
        expected.sort_unstable();
        let bound: Vec<String> = db
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .filter(|s| s.sql.contains("SET dep_counts_version"))
            .map(|s| format!("{:?}", s.values))
            .collect();
        let stamp = bound.first().expect("the stamp is written");
        let first = stamp
            .find(&expected[0].to_string())
            .expect("the lower id is bound");
        let second = stamp
            .find(&expected[1].to_string())
            .expect("the higher id is bound");

        assert!(first < second, "{stamp}");
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
        assert!(
            log[0].contains("ORDER BY id FOR NO KEY UPDATE"),
            "{}",
            log[0]
        );
    }
}
