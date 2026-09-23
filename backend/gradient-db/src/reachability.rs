/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation <-> derivation reachability over the global graph. A `build_job`
//! row is how an evaluation names a derivation it waits on, and every reader of
//! "does some evaluation still want this" (the promotion gate, the dispatch
//! select, the dispatcher's driving evaluation, eval-done, the abort's shared
//! set) reads the row rather than walking the graph. A walk names what it
//! recorded plus the direct inputs of that, so the interior of a pruned subtree
//! is named by the evaluation that walked it and by nobody else; adoption is what
//! keeps the rows true when that evaluation is deleted, or a thaw, a reset or a
//! retire leaves an open anchor unnamed, while another evaluation still waits on
//! the subtree.

use crate::graph_sql::{builder_predicate, open_closure_cte, open_predicate};
use crate::status_sql;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseTransaction, DbErr, EntityTrait, QueryFilter,
    QuerySelect, TransactionTrait, Value,
};
use std::sync::LazyLock;

/// The anchor's current status, for the dispatcher's last look before a
/// hand-out: a queued job whose gate regressed since it was enqueued reads
/// `Created` here and is dropped instead of dispatched with a missing input.
pub async fn anchor_status<C: ConnectionTrait>(
    db: &C,
    anchor: DerivationBuildId,
) -> Result<Option<BuildStatus>, DbErr> {
    Ok(EDerivationBuild::find_by_id(anchor)
        .one(db)
        .await?
        .map(|a| a.status))
}

/// The anchor's status AND whether it is still a relay, in one read: the
/// dispatcher assembles a job from a snapshot, and an upstream probe that lands
/// before the hand-out changes which of the two a worker should be given.
pub async fn anchor_dispatch_state<C: ConnectionTrait>(
    db: &C,
    anchor: DerivationBuildId,
) -> Result<Option<(BuildStatus, bool)>, DbErr> {
    Ok(EDerivationBuild::find_by_id(anchor)
        .one(db)
        .await?
        .map(|a| (a.status, a.substitutable)))
}

/// Evaluations that reference `derivation` (via a `build_job`). Drives status
/// fan-out: a single anchor transition updates every referencing eval's view.
pub async fn evals_referencing_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<EvaluationId>, DbErr> {
    EBuildJob::find()
        .select_only()
        .column(CBuildJob::Evaluation)
        .distinct()
        .filter(CBuildJob::Derivation.eq(derivation))
        .into_tuple::<EvaluationId>()
        .all(db)
        .await
}

/// Bulk variant of [`evals_referencing_derivation`]: one chunked `IN` per batch
/// instead of a round-trip per derivation. The finalize fan-out asks for a whole
/// batch of derivations at once and only ever wants the union.
pub async fn evals_referencing_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Vec<EvaluationId>, DbErr> {
    let mut all = crate::fetch_in_chunks(derivations, |chunk| async move {
        EBuildJob::find()
            .select_only()
            .column(CBuildJob::Evaluation)
            .distinct()
            .filter(CBuildJob::Derivation.is_in(chunk))
            .into_tuple::<EvaluationId>()
            .all(db)
            .await
    })
    .await?;
    all.sort_unstable();
    all.dedup();

    Ok(all)
}

/// All `build_job` rows for `derivation`, across every evaluation that needs it.
pub async fn build_jobs_for_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Derivation.eq(derivation))
        .all(db)
        .await
}

/// Bulk variant of [`build_jobs_for_derivation`]: one IN-list query for the
/// whole batch instead of a round-trip per derivation.
pub async fn build_jobs_for_derivations<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<std::collections::HashMap<DerivationId, Vec<MBuildJob>>, DbErr> {
    Ok(crate::fetch_in_chunks(derivations, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await?
    .into_iter()
    .fold(std::collections::HashMap::new(), |mut m, j| {
        m.entry(j.derivation).or_default().push(j);
        m
    }))
}

crate::sql! {
    PRODUCERS_OF_HASHES = "SELECT DISTINCT o.derivation FROM derivation_output o WHERE o.hash = ANY($1)",
        params = [CachedPathHashes(64)];

    /// A restart re-runs the previous evaluation's graph without walking it, so it
    /// takes the previous names over as its own: every reader of "does some
    /// evaluation still want this", and the thaw's seed, is a `build_job` row.
    INHERIT_NAMES = "INSERT INTO build_job \
         (id, evaluation, derivation, derivation_build, score, score_breakdown, created_at) \
         SELECT uuidv7(), $2, bj.derivation, bj.derivation_build, 0, '{}'::jsonb, \
         (now() AT TIME ZONE 'UTC') \
         FROM build_job bj WHERE bj.evaluation = $1 \
         ON CONFLICT (evaluation, derivation) DO NOTHING",
        params = [EvaluationId, EvaluationId],
        tier = Bulk;
}

/// Name for `to` everything `from` names. Returns how many names it took over.
pub async fn inherit_names<C: ConnectionTrait>(
    db: &C,
    from: EvaluationId,
    to: EvaluationId,
) -> Result<u64, DbErr> {
    Ok(db
        .execute_raw(INHERIT_NAMES.bind([
            Value::Uuid(Some(from.into_inner())),
            Value::Uuid(Some(to.into_inner())),
        ]))
        .await?
        .rows_affected())
}

/// The derivations whose outputs carry any of `hashes`: the anchors a store
/// path's arrival or removal can make fetchable or unfetchable.
pub async fn producers_of_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(PRODUCERS_OF_HASHES.bind([hashes.to_vec().into()]))
        .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect())
}

crate::sql! {
    DERIVATIONS_WITH_HASHES = "SELECT d.id FROM derivation d WHERE d.hash = ANY($1)",
        params = [DerivationHashes(64)];
}

/// The derivations whose own `.drv` hash is any of `hashes`: the anchors whose
/// own derivation file just arrived in, or left, the cache.
pub async fn derivations_with_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<Vec<DerivationId>, DbErr> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let rows = db
        .query_all_raw(DERIVATIONS_WITH_HASHES.bind([hashes.to_vec().into()]))
        .await?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "id").ok())
        .map(DerivationId::new)
        .collect())
}

/// Whether any surviving evaluation needs `derivation` (a `build_job` exists).
/// The refcount source for derivation GC.
pub async fn derivation_is_reachable<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<bool, DbErr> {
    Ok(EBuildJob::find()
        .filter(CBuildJob::Derivation.eq(derivation))
        .limit(1)
        .one(db)
        .await?
        .is_some())
}

/// The `build_job` rows an adoption pass inserted, as `(evaluation, derivation)`.
#[derive(Debug, Default, PartialEq)]
pub struct Adopted {
    pub pairs: Vec<(EvaluationId, DerivationId)>,
}

impl Adopted {
    /// The evaluations that took a name on, deduplicated: their build list moved.
    pub fn evaluations(&self) -> Vec<EvaluationId> {
        let mut out: Vec<EvaluationId> = self.pairs.iter().map(|(e, _)| *e).collect();
        out.sort_unstable();
        out.dedup();

        out
    }

    /// The anchors that gained a name, deduplicated: what a promote re-checks.
    pub fn derivations(&self) -> Vec<DerivationId> {
        let mut out: Vec<DerivationId> = self.pairs.iter().map(|(_, d)| *d).collect();
        out.sort_unstable();
        out.dedup();

        out
    }
}

/// The walk's seeds: one row per `build_job` on an open anchor, carrying the
/// builder bit of the row it stands on.
fn named_open(scope: &str) -> String {
    format!(
        "SELECT bj.evaluation, bj.derivation, ({builder}) FROM build_job bj \
         JOIN derivation_build db ON db.derivation = bj.derivation \
         JOIN derivation w ON w.id = db.derivation \
         WHERE {scope} AND {open}",
        builder = builder_predicate("db", "w"),
        open = open_predicate("db"),
    )
}

fn adopt_sql(seed_select: &str) -> String {
    format!(
        "{cte} INSERT INTO build_job \
         (id, evaluation, derivation, derivation_build, score, score_breakdown, created_at) \
         SELECT uuidv7(), p.evaluation, p.derivation, db.id, 0, '{{}}'::jsonb, \
         (now() AT TIME ZONE 'UTC') \
         FROM pending p JOIN derivation_build db ON db.derivation = p.derivation \
         ON CONFLICT (evaluation, derivation) DO NOTHING \
         RETURNING evaluation, derivation",
        cte = open_closure_cte("pending", seed_select),
    )
}

/// Every live evaluation names what it reaches: the GC and the sweep run this.
static ADOPT_LIVE: LazyLock<String> = LazyLock::new(|| {
    adopt_sql(&named_open(&format!(
        "EXISTS (SELECT 1 FROM evaluation ev WHERE ev.id = bj.evaluation AND ev.status IN ({live}))",
        live = status_sql::eval_in(&EvaluationStatus::ACTIVE),
    )))
});

/// One evaluation names what it reaches: the graph reconciler runs this for the
/// evaluation it heals, whatever its status.
static ADOPT_EVAL: LazyLock<String> =
    LazyLock::new(|| adopt_sql(&named_open("bj.evaluation = $1")));

crate::sql_lazy! {
    ADOPT_LIVE_QUERY = || ADOPT_LIVE.as_str(),
        params = [],
        tier = Walk,
        flags = [Walk];
}

crate::sql_lazy! {
    ADOPT_EVAL_QUERY = || ADOPT_EVAL.as_str(),
        params = [EvaluationId],
        tier = Walk,
        flags = [Walk];
}

fn pending_orphans_sql(scope: &str) -> String {
    format!(
        "SELECT 1 FROM derivation_build db WHERE {scope}{open} \
         AND NOT EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) LIMIT 1",
        open = open_predicate("db"),
    )
}

static PENDING_ORPHANS_AMONG: LazyLock<String> =
    LazyLock::new(|| pending_orphans_sql("db.derivation = ANY($1::uuid[]) AND "));

crate::sql_lazy! {
    PENDING_ORPHANS_AMONG_QUERY = || PENDING_ORPHANS_AMONG.as_str(),
        params = [DerivationIds(64)];
}

/// The frontier every naming hole has: an open anchor nobody names, one edge the
/// walk would take below an open anchor a live evaluation names. Asked on every
/// sweep, and the walk runs only when the answer is yes; a settled server has no
/// frontier, so the answer costs a pass over the anchors that are still open.
static PENDING_ORPHAN_FRONTIER: LazyLock<String> = LazyLock::new(|| {
    pending_orphans_sql(&format!(
        "EXISTS (SELECT 1 FROM derivation_dependency e \
         JOIN derivation_build p ON p.derivation = e.derivation \
         JOIN derivation w ON w.id = p.derivation \
         JOIN build_job pj ON pj.derivation = p.derivation \
         JOIN evaluation ev ON ev.id = pj.evaluation \
         WHERE e.dependency = db.derivation AND {open} \
           AND (({builder}) OR e.kind IN (1, 2)) AND ev.status IN ({live})) AND ",
        open = open_predicate("p"),
        builder = builder_predicate("p", "w"),
        live = status_sql::eval_in(&EvaluationStatus::ACTIVE),
    ))
});

crate::sql_lazy! {
    PENDING_ORPHAN_FRONTIER_QUERY = || PENDING_ORPHAN_FRONTIER.as_str(),
        params = [],
        tier = Sweep;
}

/// Whether any of `derivations` is open with no `build_job` left: what the
/// per-task GC asks about the names it just cascaded away, before it pays for
/// the walk.
pub async fn pending_orphans_among<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<bool, DbErr> {
    if derivations.is_empty() {
        return Ok(false);
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    Ok(db
        .query_one_raw(PENDING_ORPHANS_AMONG_QUERY.bind([ids.into()]))
        .await?
        .is_some())
}

/// Whether some live evaluation reaches an open anchor nobody names: the
/// consistency sweep's guard on the walk.
pub async fn pending_orphan_frontier<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    Ok(db
        .query_one_raw(PENDING_ORPHAN_FRONTIER_QUERY.stmt())
        .await?
        .is_some())
}

/// Name, for every live evaluation, each open anchor it reaches from the open
/// anchors it already names, and return the rows that were missing. One statement
/// under the walk's own transaction; a concurrent ingest naming the same pair is
/// absorbed by the conflict clause.
pub async fn adopt_pending_closures<C>(db: &C) -> Result<Adopted, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    adopt(db, &ADOPT_LIVE_QUERY, []).await
}

/// [`adopt_pending_closures`] for one evaluation, from its own names.
pub async fn adopt_pending_closure<C>(db: &C, evaluation: EvaluationId) -> Result<Adopted, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    adopt(
        db,
        &ADOPT_EVAL_QUERY,
        [Value::Uuid(Some(evaluation.into_inner()))],
    )
    .await
}

async fn adopt<C, V>(db: &C, query: &crate::sql::Query, values: V) -> Result<Adopted, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
    V: IntoIterator<Item = Value>,
{
    let walk = crate::graph_sql::begin_walk(db).await?;
    let rows = walk.query_all_raw(query.bind(values)).await?;
    walk.commit().await?;

    let pairs = rows
        .iter()
        .map(|r| {
            Ok((
                EvaluationId::new(r.try_get::<uuid::Uuid>("", "evaluation")?),
                DerivationId::new(r.try_get::<uuid::Uuid>("", "derivation")?),
            ))
        })
        .collect::<Result<Vec<_>, DbErr>>()?;

    Ok(Adopted { pairs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph_sql::WALK_WORK_MEM;
    use crate::pool::statements;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn exec() -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: 0,
        }
    }

    fn pair(e: EvaluationId, d: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("evaluation".to_owned(), Value::from(e.into_inner())),
            ("derivation".to_owned(), Value::from(d.into_inner())),
        ])
    }

    /// Adoption is one statement under the walk's own transaction: the open
    /// closure below every open anchor a live evaluation names, inserted as the
    /// names it lacks and returned as such. The seed is what a live evaluation
    /// NAMES and is still open, so a pruned root and an unwhole `Completed` input
    /// seed the walk like a builder does.
    #[tokio::test]
    async fn adoption_names_every_open_anchor_a_live_evaluation_reaches() {
        let e = EvaluationId::now_v7();
        let d = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec()])
            .append_query_results([vec![pair(e, d)]])
            .into_connection();

        let adopted = adopt_pending_closures(&db).await.unwrap();

        assert_eq!(adopted.pairs, vec![(e, d)]);
        assert_eq!(adopted.evaluations(), vec![e]);
        assert_eq!(adopted.derivations(), vec![d]);
        let log = statements(db.into_transaction_log());
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(log[0].contains(WALK_WORK_MEM), "{log:?}");
        let sql = norm(&log[1]);
        assert!(
            sql.contains(
                "WITH RECURSIVE pending(evaluation, derivation, builder) AS \
                 (SELECT bj.evaluation, bj.derivation, (w.walked AND db.probed \
                 AND NOT db.substitutable AND db.status IN (0, 1, 2, 8)) FROM build_job bj"
            ),
            "{sql}"
        );
        assert!(
            sql.contains(&format!(
                "WHERE EXISTS (SELECT 1 FROM evaluation ev WHERE ev.id = bj.evaluation \
                 AND ev.status IN ({})) AND (NOT db.fetchable AND db.status NOT IN (4, 6, 9))",
                status_sql::eval_in(&EvaluationStatus::ACTIVE)
            )),
            "{sql}"
        );
        assert!(
            sql.contains(
                "INSERT INTO build_job (id, evaluation, derivation, derivation_build, \
                 score, score_breakdown, created_at) \
                 SELECT uuidv7(), p.evaluation, p.derivation, db.id, 0, '{}'::jsonb, \
                 (now() AT TIME ZONE 'UTC') \
                 FROM pending p JOIN derivation_build db ON db.derivation = p.derivation \
                 ON CONFLICT (evaluation, derivation) DO NOTHING \
                 RETURNING evaluation, derivation"
            ),
            "{sql}"
        );
    }

    /// The reconciler's variant is the same walk seeded from one evaluation's
    /// names and without the liveness filter: it runs for the evaluation it heals.
    #[tokio::test]
    async fn one_evaluation_adopts_from_its_own_names() {
        let e = EvaluationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        assert!(
            adopt_pending_closure(&db, e)
                .await
                .unwrap()
                .pairs
                .is_empty()
        );
        let log = statements(db.into_transaction_log());
        let sql = norm(&log[1]);
        assert!(
            sql.contains(
                "FROM build_job bj JOIN derivation_build db ON db.derivation = bj.derivation \
                 JOIN derivation w ON w.id = db.derivation WHERE bj.evaluation = $1 \
                 AND (NOT db.fetchable"
            ),
            "{sql}"
        );
        assert!(!sql.contains("FROM evaluation ev"), "{sql}");
    }

    /// The GC's question is bounded to the names it cascaded away and reads one
    /// row at most; the sweep's is the frontier every naming hole has: an open
    /// anchor nobody names, one edge the walk would take below an open anchor a
    /// live evaluation names. The 22 unwhole `Completed` anchors of the wedge were
    /// exactly that and matched neither probe while both asked for a status.
    #[tokio::test]
    async fn the_orphan_probes_read_one_row_and_bind_their_scope() {
        let d = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([(
                "?column?".to_owned(),
                Value::Int(Some(1)),
            )])]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();

        assert!(pending_orphans_among(&db, &[d]).await.unwrap());
        assert!(!pending_orphan_frontier(&db).await.unwrap());
        assert!(
            !pending_orphans_among(&db, &[]).await.unwrap(),
            "an empty scope asks nothing"
        );
        let log = statements(db.into_transaction_log());
        assert_eq!(log.len(), 2, "{log:?}");
        for sql in &log {
            let sql = norm(sql);
            assert!(
                sql.contains(
                    "(NOT db.fetchable AND db.status NOT IN (4, 6, 9)) AND NOT EXISTS \
                     (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) LIMIT 1"
                ),
                "{sql}"
            );
        }

        assert!(
            norm(&log[0]).contains("db.derivation = ANY($1::uuid[])"),
            "{log:?}"
        );
        let frontier = norm(&log[1]);
        assert!(
            frontier.contains(
                "JOIN build_job pj ON pj.derivation = p.derivation \
                 JOIN evaluation ev ON ev.id = pj.evaluation \
                 WHERE e.dependency = db.derivation \
                 AND (NOT p.fetchable AND p.status NOT IN (4, 6, 9)) \
                 AND ((w.walked AND p.probed AND NOT p.substitutable \
                 AND p.status IN (0, 1, 2, 8)) OR e.kind IN (1, 2)) AND ev.status IN ("
            ),
            "{frontier}"
        );
    }
}
