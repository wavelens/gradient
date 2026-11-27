/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::predicates::{
    aborted_dependency_without_failure_spread_predicate, builds_once_wanted_predicate,
    open_predicate,
};
use crate::graph::promotion::{returned_derivations, returned_transitions};
use crate::sql::StatusWrite;
use crate::status::TransitionChange;

use super::lock::{ids, lock_shared_builds};
use super::queue::{promote, unpromote_ungated};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::DerivationId;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, QueryResult, TransactionTrait};
use std::collections::HashSet;
use std::sync::LazyLock;

const IN_CANDIDATES: &str = "db.derivation = ANY($1::uuid[]) AND ";

fn skip_unwanted_sql(scope: &str) -> String {
    format!(
        "UPDATE derivation_build db SET status = {to}, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE {scope}db.status = {from} AND NOT db.wanted \
           AND NOT EXISTS (SELECT 1 FROM entry_point ep WHERE ep.derivation = db.derivation) \
         RETURNING db.derivation, {from} AS from_status, {to} AS to_status",
        from = crate::sql::status::from(BuildStatus::SKIP_UNWANTED),
        to = crate::sql::status::to(BuildStatus::SKIP_UNWANTED),
    )
}

fn thaw_wanted_sql(scope: &str) -> String {
    format!(
        "UPDATE derivation_build db SET status = {to}, attempt = 0, updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE {scope}db.status IN ({from}) AND db.wanted \
         RETURNING db.derivation, old.status AS from_status, {to} AS to_status",
        from = crate::sql::status::from(BuildStatus::THAW_WANTED),
        to = crate::sql::status::to(BuildStatus::THAW_WANTED),
    )
}

fn forget_misses_from_before_its_first_evaluation_sql() -> String {
    format!(
        "UPDATE derivation_build db SET probed = false, probed_at = NULL, \
         updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE db.derivation = ANY($1::uuid[]) AND db.probed AND NOT db.cache_available \
           AND db.status IN ({idle}) \
           AND db.probed_at < (SELECT min(ev.created_at) FROM build_job bj \
                               JOIN evaluation ev ON ev.id = bj.evaluation \
                               WHERE bj.derivation = db.derivation) \
         RETURNING db.derivation",
        idle = crate::sql::status::build_in(&[BuildStatus::Created, BuildStatus::Skipped]),
    )
}

crate::sql_fn! {
    FORGET_MISSES_FROM_BEFORE_ITS_FIRST_EVALUATION = forget_misses_from_before_its_first_evaluation_sql,
        params = [DerivationIds(64)];

    SKIP_UNWANTED = || skip_unwanted_sql(IN_CANDIDATES),
        params = [DerivationIds(64)],
        status_write = StatusWrite::build(BuildStatus::SKIP_UNWANTED);

    THAW_WANTED = || thaw_wanted_sql(IN_CANDIDATES),
        params = [DerivationIds(64)],
        status_write = StatusWrite::build(BuildStatus::THAW_WANTED);

    SKIP_UNWANTED_ALL = || skip_unwanted_sql(""),
        params = [],
        tier = Sweep,
        status_write = StatusWrite::build(BuildStatus::SKIP_UNWANTED);

    THAW_WANTED_ALL = || thaw_wanted_sql(""),
        params = [],
        tier = Sweep,
        status_write = StatusWrite::build(BuildStatus::THAW_WANTED);
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
        "WITH RECURSIVE {cte}, \
         reached AS MATERIALIZED (SELECT DISTINCT derivation FROM wanted) \
         UPDATE derivation_build db \
         SET wanted = x.wanted, updated_at = (now() AT TIME ZONE 'UTC') \
         FROM (SELECT s.derivation, (r.derivation IS NOT NULL) AS wanted \
               FROM derivation_build s LEFT JOIN reached r ON r.derivation = s.derivation \
               WHERE ({open} OR s.fetchable)) x \
         WHERE db.derivation = x.derivation AND db.wanted <> x.wanted \
         RETURNING db.derivation, db.wanted",
        cte = crate::graph::walks::need_walk_dependency_recursive_cte_body_until_after_index_lookup(
            "wanted",
            &open_entry_points(),
            None,
            &|step| aborted_dependency_without_failure_spread_predicate(step, step, None),
        ),
        open = open_predicate("s"),
    )
});

/// An aborted evaluation and an aborted entry point want nothing. Aborted shared builds are
/// open, and such entry points kept them wanted and thawed. Completed and failed evaluations
/// still want a retired output built again.
pub(crate) fn wanted_by_its_evaluation(entry_point: &str) -> String {
    format!(
        "JOIN evaluation ev ON ev.id = {entry_point}.evaluation AND ev.status <> {aborted} \
         AND (NOT ev.only_evaluate OR EXISTS (SELECT 1 FROM derivation entry_point_derivation \
              WHERE entry_point_derivation.id = {entry_point}.derivation \
                AND entry_point_derivation.ifd)) \
         AND NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.evaluation = {entry_point}.evaluation \
                         AND bj.derivation = {entry_point}.derivation AND bj.aborted)",
        aborted = crate::sql::status::eval(EvaluationStatus::Aborted),
    )
}

fn open_entry_points() -> String {
    format!(
        "SELECT NULL::uuid, db.derivation, ({builder}) FROM entry_point ep {live} \
         JOIN derivation_build db ON db.derivation = ep.derivation \
         JOIN derivation w ON w.id = db.derivation WHERE {open}",
        live = wanted_by_its_evaluation("ep"),
        builder = builds_once_wanted_predicate("db", "w"),
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

static UPDATE_NEED_SQL: LazyLock<String> = LazyLock::new(|| need_sql(""));

static GAIN_NEED_SQL: LazyLock<String> = LazyLock::new(|| need_sql("dep.wanted"));

fn walk_may_enter_dependency_sql() -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM derivation_build db \
                     WHERE db.derivation = k.dependency AND {aborted_without_spread})",
        aborted_without_spread =
            aborted_dependency_without_failure_spread_predicate("k", "db", Some("pb")),
    )
}

fn need_sql(stop_region_at: &str) -> String {
    let region = crate::graph::walks::need_closure_cte_body_until(
        "region",
        "SELECT NULL::uuid AS evaluation, unnest($1::uuid[]) AS derivation, true AS builder",
        None,
        stop_region_at,
    );
    let entered = format!(
        "region_derivation_id_array(region_derivation_ids) AS MATERIALIZED \
         (SELECT array_agg(DISTINCT derivation) FROM region), \
         entered(derivation) AS (SELECT r.derivation \
         FROM (SELECT DISTINCT derivation FROM region) r \
         CROSS JOIN LATERAL (SELECT 1 FROM \
          (SELECT e.derivation AS parent FROM derivation_dependency e \
           CROSS JOIN LATERAL (SELECT 1 FROM derivation_build p \
                               WHERE p.derivation = e.derivation AND p.wanted AND {open} \
                               OFFSET 0) wanted_open_parent \
           WHERE e.dependency = r.derivation \
             AND e.derivation NOT IN (SELECT unnest(region_derivation_ids) \
                                      FROM region_derivation_id_array) \
           OFFSET 0) wanted_open_outside_region_parent_derivations \
          CROSS JOIN LATERAL (SELECT 1 FROM derivation_dependency k \
                              JOIN derivation_build pb ON pb.derivation = k.derivation \
                              JOIN derivation pw ON pw.id = pb.derivation \
                              WHERE (k.derivation, k.dependency) \
                                    = (wanted_open_outside_region_parent_derivations.parent, r.derivation) \
                                AND (({builder}) OR k.kind IN (1, 2)) \
                                AND {walk_may_enter_dependency_sql} OFFSET 0) outside_parent_wanted_region_derivation_edge \
          LIMIT 1) wanting)",
        open = open_predicate("p"),
        builder = builds_once_wanted_predicate("pb", "pw"),
        walk_may_enter_dependency_sql = walk_may_enter_dependency_sql(),
    );
    let seeds = format!(
        "seeds(derivation) AS MATERIALIZED (SELECT r.derivation \
         FROM (SELECT DISTINCT derivation FROM region) r \
         WHERE EXISTS (SELECT 1 FROM entry_point ep {live} WHERE ep.derivation = r.derivation) \
         UNION SELECT derivation FROM entered)",
        live = wanted_by_its_evaluation("ep"),
    );
    let seed = format!(
        "SELECT NULL::uuid, s.derivation, b.builder FROM seeds s, \
         LATERAL (SELECT ({builder}) AS builder FROM derivation_build rb \
                  JOIN derivation w ON w.id = rb.derivation \
                  WHERE rb.derivation = s.derivation AND {open} OFFSET 0) b",
        builder = builds_once_wanted_predicate("rb", "w"),
        open = open_predicate("rb"),
    );
    let wanted =
        crate::graph::walks::need_walk_dependency_recursive_cte_body_until_after_index_lookup(
            "wanted",
            &seed,
            Some("region"),
            &|step| aborted_dependency_without_failure_spread_predicate(step, step, None),
        );

    format!(
        "WITH RECURSIVE {region}, {entered}, {seeds}, {wanted} \
         SELECT DISTINCT r.derivation, (d.derivation IS NOT NULL) AS wanted \
         FROM region r LEFT JOIN (SELECT DISTINCT derivation FROM wanted) d \
           ON d.derivation = r.derivation ORDER BY r.derivation",
    )
}

crate::sql_lazy! {
    UPDATE_NEED = || UPDATE_NEED_SQL.as_str(),
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk, DefaultWorkMem];

    GAIN_NEED = || GAIN_NEED_SQL.as_str(),
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

#[tracing::instrument(level = "debug", skip_all, fields(rows = region.len()))]
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
    walk_need(db, &UPDATE_NEED, roots).await
}

pub(crate) async fn gain_need<C>(db: &C, roots: &[DerivationId]) -> Result<NeedMoved, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    walk_need(db, &GAIN_NEED, roots).await
}

#[tracing::instrument(level = "debug", skip_all, fields(walk = walk.name, roots = roots.len()))]
async fn walk_need<C>(
    db: &C,
    walk: &crate::sql::Query,
    roots: &[DerivationId],
) -> Result<NeedMoved, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if roots.is_empty() {
        return Ok(NeedMoved::default());
    }

    let txn = crate::graph::walks::begin_walk(db, walk).await?;
    let _lock = lock_shared_builds(&txn, roots).await?;
    let region = txn.query_all_raw(walk.bind([ids(roots)])).await?;
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

pub(crate) async fn settle_need<C>(
    db: &C,
    moved: &NeedMoved,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    Ok(settle_need_with_limit(db, moved, usize::MAX).await?.changes)
}

/// Order is load-bearing on both sides.
/// The thaw and the adoption of jobs must precede the promote, which needs a builder and a job.
/// The skip must follow the un-promote, or it would try to settle a row still `Queued`.
#[tracing::instrument(level = "debug", skip_all, fields(gained = moved.gained.len(), lost = moved.lost.len()))]
async fn settle_need_with_limit<C>(
    db: &C,
    moved: &NeedMoved,
    derivation_revisit_limit: usize,
) -> Result<BudgetedNeedUpdate, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut changes = Vec::new();
    for gained in moved.gained.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(thaw_wanted(db, gained).await?);
        name_below_named(db, gained).await?;
    }
    let mut forgotten = forget_misses_recorded_before_first_evaluation(db, &moved.gained).await?;
    let deferred_rewalk_derivations =
        forgotten.split_off(derivation_revisit_limit.min(forgotten.len()));
    let asked_again = update_need(db, &forgotten).await?;
    let wanted_derivation_ids_to_promote_after_deferred_rewalks =
        if deferred_rewalk_derivations.is_empty() {
            for gained in moved.gained.chunks(crate::IN_CHUNK_SIZE) {
                changes.extend(promote(db, gained).await?);
            }
            Vec::new()
        } else {
            moved.gained.clone()
        };
    changes.extend(settle_lost(db, &moved.lost).await?);
    changes.extend(settle_lost(db, &asked_again.lost).await?);

    Ok(BudgetedNeedUpdate {
        changes,
        deferred_rewalk_derivations,
        wanted_derivation_ids_to_promote_after_deferred_rewalks,
    })
}

async fn forget_misses_recorded_before_first_evaluation<C: ConnectionTrait>(
    db: &C,
    gained: &[DerivationId],
) -> Result<Vec<DerivationId>, DbErr> {
    let mut forgotten = Vec::new();
    for chunk in gained.chunks(crate::IN_CHUNK_SIZE) {
        forgotten.extend(returned_derivations(
            db.query_all_raw(FORGET_MISSES_FROM_BEFORE_ITS_FIRST_EVALUATION.bind([ids(chunk)]))
                .await?,
        ));
    }

    Ok(forgotten)
}

async fn ask_again_about_earlier_misses<C>(
    db: &C,
    gained: &[DerivationId],
) -> Result<NeedMoved, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let forgotten = forget_misses_recorded_before_first_evaluation(db, gained).await?;
    update_need(db, &forgotten).await
}

async fn name_below_named<C>(db: &C, gained: &[DerivationId]) -> Result<(), DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut evaluations = Vec::new();
    loop {
        let adopted = crate::graph::reachability::adopt_below_named(db, gained).await?;
        if adopted.pairs.is_empty() {
            break;
        }
        evaluations.extend(adopted.evaluations());
    }
    evaluations.sort_unstable();
    evaluations.dedup();
    crate::task_board::dep_counts::bump_graph_version(db, &evaluations).await?;

    Ok(())
}

async fn settle_lost<C: ConnectionTrait>(
    db: &C,
    lost: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let mut changes = Vec::new();
    for lost in lost.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(unpromote_ungated(db, lost).await?);
        changes.extend(skip_unwanted(db, lost).await?);
    }

    Ok(changes)
}

pub(crate) async fn settle_batch_need<C>(
    db: &C,
    roots: &[DerivationId],
    moved: &NeedMoved,
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let mut changes = thaw_wanted(db, &moved.gained).await?;
    let mut below = roots.to_vec();
    below.extend_from_slice(&moved.gained);
    below.sort_unstable();
    below.dedup();
    let adopted = crate::graph::reachability::adopt_unnamed_below(db, &below).await?;
    crate::task_board::dep_counts::bump_graph_version(db, &adopted.evaluations()).await?;
    let asked_again = ask_again_about_earlier_misses(db, &moved.gained).await?;
    changes.extend(promote(db, &moved.gained).await?);
    changes.extend(settle_lost(db, &moved.lost).await?);
    changes.extend(settle_lost(db, &asked_again.lost).await?);

    Ok(changes)
}

#[tracing::instrument(level = "debug", skip_all, fields(candidates = derivations.len()))]
pub async fn settle_created<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr> {
    let mut changes = Vec::new();
    for chunk in derivations.chunks(crate::IN_CHUNK_SIZE) {
        changes.extend(skip_unwanted(db, chunk).await?);
        changes.extend(promote(db, chunk).await?);
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

pub async fn gain_and_settle_need<C>(db: &C, roots: &[DerivationId]) -> Result<SettledNeed, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let moved = gain_need(db, roots).await?;
    let changes = settle_batch_need(db, roots, &moved).await?;

    Ok(SettledNeed { moved, changes })
}

pub async fn gain_and_update_and_settle_need<C>(
    db: &C,
    misses: &[DerivationId],
    hits: &[DerivationId],
) -> Result<SettledNeed, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let updated = update_need(db, hits).await?;
    let gained = gain_need(db, misses).await?;
    let lost: HashSet<DerivationId> = updated.lost.iter().copied().collect();
    let lost_then_gained: HashSet<DerivationId> = gained
        .gained
        .iter()
        .filter(|d| lost.contains(d))
        .copied()
        .collect();
    let moved = NeedMoved {
        gained: updated
            .gained
            .into_iter()
            .chain(gained.gained)
            .filter(|d| !lost_then_gained.contains(d))
            .collect(),
        lost: updated
            .lost
            .into_iter()
            .chain(gained.lost)
            .filter(|d| !lost_then_gained.contains(d))
            .collect(),
    };
    let changes = settle_need(db, &moved).await?;

    Ok(SettledNeed { moved, changes })
}

#[derive(Debug, Default)]
pub struct BudgetedNeedUpdate {
    pub changes: Vec<TransitionChange>,
    pub deferred_rewalk_derivations: Vec<DerivationId>,
    pub wanted_derivation_ids_to_promote_after_deferred_rewalks: Vec<DerivationId>,
}

pub async fn update_and_settle_need_with_advisory_key_limit<C>(
    db: &C,
    roots: &[DerivationId],
    max_advisory_keys: usize,
) -> Result<BudgetedNeedUpdate, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let moved = update_need(db, roots).await?;
    settle_need_with_limit(db, &moved, max_advisory_keys.saturating_sub(roots.len())).await
}

pub async fn recheck_forgotten_need<C>(
    db: &C,
    forgotten: &[DerivationId],
) -> Result<Vec<TransitionChange>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    let asked_again = update_need(db, forgotten).await?;
    settle_lost(db, &asked_again.lost).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::can_start::test_rows::{drv, exec, transition_row};
    use gradient_entity::build::BuildStatus;
    use gradient_fixtures::mock::mock_db;
    use sea_orm::Value;
    use std::collections::BTreeMap;

    fn need_row(id: DerivationId, wanted: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(id.into_inner())),
            ("wanted".to_owned(), Value::from(wanted)),
        ])
    }

    #[tokio::test]
    async fn a_need_move_thaws_what_gained_it_and_skips_what_lost_it() {
        let root = DerivationId::now_v7();
        let on = DerivationId::now_v7();
        let off = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = mock_db()
            .append_exec_results([exec(0), exec(1), exec(0)])
            .append_query_results([vec![need_row(on, true), need_row(off, false)]])
            .append_query_results([vec![need_row(on, true), need_row(off, false)]])
            .append_query_results([
                vec![transition_row(on, 10, 0)],
                empty.clone(),
                empty.clone(),
                vec![drv(on)],
            ])
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
    }

    #[tokio::test]
    async fn a_batch_move_names_below_its_roots_and_below_what_gained() {
        let root = DerivationId::now_v7();
        let on = DerivationId::now_v7();
        let off = DerivationId::now_v7();
        let empty = Vec::<BTreeMap<String, Value>>::new();
        let db = mock_db()
            .append_exec_results([exec(0), exec(1), exec(0)])
            .append_query_results([vec![need_row(on, true), need_row(off, false)]])
            .append_query_results([vec![need_row(on, true), need_row(off, false)]])
            .append_query_results([
                vec![transition_row(on, 10, 0)],
                empty.clone(),
                empty.clone(),
                vec![drv(on)],
            ])
            .append_query_results([empty, vec![transition_row(off, 0, 10)]])
            .into_connection();

        let settled = gain_and_settle_need(&db, &[root]).await.unwrap();

        assert_eq!(settled.moved.gained, vec![on]);
        assert_eq!(settled.moved.lost, vec![off]);
        let log = db.into_transaction_log();
        let adopt = log
            .iter()
            .flat_map(|t| t.statements())
            .find(|s| s.sql.contains("INSERT INTO build_job"))
            .expect("the batch names what it reaches")
            .clone();
        let mut below: Vec<uuid::Uuid> = vec![root.into_inner(), on.into_inner()];
        below.sort_unstable();
        assert_eq!(
            adopt.values.map(|v| v.0),
            Some(vec![Value::from(below)]),
            "the roots and the gained shared build seed the naming walk together"
        );
    }
}
