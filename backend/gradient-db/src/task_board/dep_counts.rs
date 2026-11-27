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
        params = [DerivationIds(64)],
        tier = Bulk;
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
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::shape::shape;

    fn drv(letter: &str) -> String {
        format!("{}-{letter}.drv", letter.repeat(32))
    }

    async fn counts(db: &TestDatabase, evaluation: EvaluationId) -> DepCounts {
        let model = db
            .rows::<EEvaluation>()
            .await
            .into_iter()
            .find(|e| e.id == evaluation)
            .expect("the evaluation exists");
        let entry_points = db.entry_points(evaluation).await;
        cached_entry_point_dep_counts(&db.connection(), &model, &entry_points)
            .await
            .unwrap()
    }

    async fn set_status(db: &TestDatabase, path: &str, status: BuildStatus) {
        let build = db.shared_build(path).await;
        db.execute(&format!(
            "UPDATE derivation_build SET status = {} WHERE id = '{}'",
            i32::from(status),
            build.id
        ))
        .await;
    }

    async fn age_stamps(db: &TestDatabase, seconds: i64) {
        db.execute(&format!(
            "UPDATE entry_point SET dep_counts_computed_at = \
             (now() AT TIME ZONE 'UTC') - interval '{seconds} seconds'"
        ))
        .await;
    }

    #[tokio::test]
    async fn test_cached_entry_point_dep_counts() {
        let evaluation = EvaluationId::now_v7();
        let db = shape()
            .evaluation(evaluation)
            .derivation(&drv("a"))
            .build_status(BuildStatus::Queued)
            .job()
            .entry_point("root")
            .derivation(&drv("b"))
            .build_status(BuildStatus::Queued)
            .job()
            .derivation(&drv("c"))
            .build_status(BuildStatus::Completed)
            .job()
            .derivation(&drv("d"))
            .build_status(BuildStatus::Queued)
            .job()
            .entry_point("leaf")
            .derivation(&drv("a"))
            .depends_on(&drv("b"))
            .depends_on(&drv("c"))
            .into_database()
            .await;
        let entry = |attr: &'static str| {
            let db = &db;
            async move {
                db.entry_points(evaluation)
                    .await
                    .into_iter()
                    .find(|e| e.eval == attr)
                    .expect("the entry point exists")
                    .id
            }
        };
        let (root, leaf) = (entry("root").await, entry("leaf").await);
        let status_of = |counts: &DepCounts, status| counts[&root].get(&status).copied();

        let computed = counts(&db, evaluation).await;
        let stored_rows = db.rows::<EEntryPointDepCount>().await.len();
        let stamped = db
            .entry_points(evaluation)
            .await
            .iter()
            .all(|e| e.dep_counts_computed_at.is_some());

        set_status(&db, &drv("b"), BuildStatus::Completed).await;
        let damped = counts(&db, evaluation).await;

        age_stamps(&db, DEP_COUNTS_MAX_AGE_SECS + 1).await;
        let aged = counts(&db, evaluation).await;
        let rows_after_age = db.rows::<EEntryPointDepCount>().await.len();

        let bumped = bump_graph_version(&db.connection(), &[evaluation])
            .await
            .unwrap();
        let bumped_for_derivation =
            bump_graph_version_for_derivations(&db.connection(), &[db.id_of(&drv("a")).await])
                .await
                .unwrap();
        let bumped_nothing = bump_graph_version(&db.connection(), &[]).await.unwrap();
        let version = db.rows::<EEvaluation>().await[0].graph_version;

        age_stamps(&db, DEP_COUNTS_REFRESH_SECS + 1).await;
        let claimed_elsewhere = {
            let model = db.rows::<EEvaluation>().await.remove(0);
            let stale = db.entry_points(evaluation).await;
            age_stamps(&db, 0).await;
            set_status(&db, &drv("c"), BuildStatus::Queued).await;
            cached_entry_point_dep_counts(&db.connection(), &model, &stale)
                .await
                .unwrap()
        };

        age_stamps(&db, DEP_COUNTS_REFRESH_SECS + 1).await;
        let version_moved = counts(&db, evaluation).await;
        db.finish().await;

        assert_eq!(
            (
                status_of(&computed, BuildStatus::Queued),
                status_of(&computed, BuildStatus::Completed)
            ),
            (Some(1), Some(1))
        );
        assert!(!computed.contains_key(&leaf), "a leaf has no counts");
        assert_eq!(stored_rows, 2);
        assert!(stamped, "a leaf is stamped without rows");
        assert_eq!(
            status_of(&damped, BuildStatus::Queued),
            Some(1),
            "a fresh stamp reads the stored counts"
        );
        assert_eq!(
            (
                status_of(&aged, BuildStatus::Completed),
                status_of(&aged, BuildStatus::Queued)
            ),
            (Some(2), None),
            "past the age ceiling the walk replaces the stored rows"
        );
        assert_eq!(rows_after_age, 1);
        assert_eq!(
            (bumped, bumped_for_derivation, bumped_nothing, version),
            (1, 1, 0, 2)
        );
        assert_eq!(
            status_of(&claimed_elsewhere, BuildStatus::Completed),
            Some(2),
            "a refresh claimed by another reader serves the stored counts"
        );
        assert_eq!(
            (
                status_of(&version_moved, BuildStatus::Completed),
                status_of(&version_moved, BuildStatus::Queued)
            ),
            (Some(1), Some(1)),
            "a moved graph version walks again"
        );
    }
}
