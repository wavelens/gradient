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
//!
//! The version alone is not a usable cache key while an evaluation builds: every
//! anchor move bumps it, so a version-only test is always stale and the page
//! walks page-size-times-closure on every poll. Staleness is therefore damped by
//! time as well ([`DEP_COUNTS_REFRESH_SECS`]), and an age ceiling
//! ([`DEP_COUNTS_MAX_AGE_SECS`]) recomputes regardless of the stamp so a bump
//! that never landed cannot freeze a histogram.

use crate::fetch_in_chunks;
use chrono::NaiveDateTime;
use gradient_entity::build::BuildStatus;
use gradient_entity::ids::{DerivationId, EntryPointId, EvaluationId};
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, DbErr, EntityTrait, QueryFilter,
    Statement, TransactionTrait,
};
use std::collections::HashMap;

pub type DepCounts = HashMap<EntryPointId, HashMap<BuildStatus, i64>>;

/// How long a histogram the graph has outgrown may still be served. The task page
/// polls every 4 s per viewer and a building evaluation bumps its version on every
/// anchor move, so without this the walk runs on every poll of every viewer, and
/// concurrent viewers of one evaluation collapse onto one walk.
///
/// It has to stay well ABOVE what the walk costs, or the recomputes overlap and
/// the queue never drains: at 15 s against a walk measured at 93 s on a
/// 74-entry-point NixOS flake, this was 80% of the database's time on its own.
/// The bar advancing every other minute is invisible against builds that take
/// minutes; a walk that cannot finish before the next one starts is not.
pub const DEP_COUNTS_REFRESH_SECS: i64 = 120;

/// Age at which rows recompute whatever their stamp says. The emitter logs and
/// swallows a failed bump (it fans out board events and CI checks that must not be
/// held up by it), so a deadlock or statement timeout would otherwise freeze a
/// histogram for good now that the reconcile hooks are gone. One walk per opened
/// page per ten minutes is the whole price, and it is paid only for pages someone
/// is actually looking at.
pub const DEP_COUNTS_MAX_AGE_SECS: i64 = 600;

/// Whether `ep`'s cached rows must be recomputed at `now`. Never-computed rows
/// always recompute; past that, a moved graph is damped by
/// [`DEP_COUNTS_REFRESH_SECS`] and [`DEP_COUNTS_MAX_AGE_SECS`] is the backstop.
fn needs_recompute(ep: &MEntryPoint, version: i64, now: NaiveDateTime) -> bool {
    let Some(computed_at) = ep.dep_counts_computed_at else {
        return true;
    };

    let age = now.signed_duration_since(computed_at).num_seconds();
    if age >= DEP_COUNTS_MAX_AGE_SECS {
        return true;
    }

    ep.dep_counts_version != Some(version) && age >= DEP_COUNTS_REFRESH_SECS
}

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

/// The histogram of every entry point on a page: served from the cache unless
/// [`needs_recompute`] says otherwise, in which case one fenced walk recomputes
/// the stale entry points and stores them under the current version. An entry
/// point with no dependencies is absent from the map.
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
    let (stale, fresh): (Vec<&MEntryPoint>, Vec<&MEntryPoint>) = entry_points
        .iter()
        .partition(|ep| needs_recompute(ep, version, now));

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
    store_entry_point_dep_counts(db, version, now, &stale_ids, &computed).await?;
    out.extend(computed);

    Ok(out)
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
    // Both arrays are sorted so two concurrent refreshes of overlapping pages
    // take the same lock order whatever the pages are.
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
        "UPDATE entry_point SET dep_counts_version = $1, dep_counts_computed_at = $2 \
         WHERE id = ANY($3::uuid[])",
        [version.into(), computed_at.into(), ids.into()],
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

    /// An entry point whose rows were computed `age_secs` ago under `stamp`.
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

    /// An entry point nothing has ever computed.
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

    /// The fresh entry point is served from its rows and never walked; the stale
    /// one is walked, its rows replaced under the version the walk ran under.
    #[tokio::test]
    async fn a_fresh_stamp_reads_the_cache_and_a_stale_one_walks_then_stores() {
        let eval = evaluation(3);
        let fresh = entry_point(eval.id, Some(3), 1);
        let stale = entry_point(eval.id, Some(2), DEP_COUNTS_REFRESH_SECS + 1);
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

    /// A leaf entry point (no dependencies) has no rows to insert but must still
    /// be stamped, or every read would walk it again.
    #[tokio::test]
    async fn a_leaf_is_stamped_without_an_insert() {
        let eval = evaluation(1);
        let leaf = never_computed(eval.id);
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

    /// While an evaluation builds, every anchor move bumps its version, so a
    /// version-only test would walk the graph on every 4 s poll of the page. Rows
    /// the graph has outgrown but that are seconds old are served as they are.
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

    /// The emitter logs and swallows a failed bump, so a matching stamp is not
    /// proof the rows are current; past the age ceiling they are recomputed
    /// anyway, which is the only thing that heals a bump that never landed.
    #[tokio::test]
    async fn rows_past_the_age_ceiling_recompute_even_with_a_matching_stamp() {
        let eval = evaluation(5);
        let ep = entry_point(eval.id, Some(5), DEP_COUNTS_MAX_AGE_SECS + 1);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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

    /// Two concurrent refreshes of overlapping pages must queue behind each other
    /// in one direction only, so every array the write binds is sorted.
    #[tokio::test]
    async fn the_store_binds_its_ids_in_sorted_order() {
        let eval = evaluation(2);
        let a = never_computed(eval.id);
        let b = never_computed(eval.id);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
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
        assert!(log[0].contains("ORDER BY id FOR UPDATE"), "{}", log[0]);
    }
}
