/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::{
    predicates::{builder_predicate, open_predicate},
    walks::open_closure_cte,
};

use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbErr, TransactionTrait, Value};
use std::sync::LazyLock;

#[derive(Debug, Default, PartialEq)]
pub struct Adopted {
    pub pairs: Vec<(EvaluationId, DerivationId)>,
}

impl Adopted {
    pub fn evaluations(&self) -> Vec<EvaluationId> {
        let mut out: Vec<EvaluationId> = self.pairs.iter().map(|(e, _)| *e).collect();
        out.sort_unstable();
        out.dedup();

        out
    }

    pub fn derivations(&self) -> Vec<DerivationId> {
        let mut out: Vec<DerivationId> = self.pairs.iter().map(|(_, d)| *d).collect();
        out.sort_unstable();
        out.dedup();

        out
    }
}

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

static ADOPT_LIVE: LazyLock<String> = LazyLock::new(|| {
    adopt_sql(&named_open(&format!(
        "EXISTS (SELECT 1 FROM evaluation ev WHERE ev.id = bj.evaluation AND ev.status IN ({live}))",
        live = crate::sql::status::eval_in(&EvaluationStatus::ACTIVE),
    )))
});

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
        live = crate::sql::status::eval_in(&EvaluationStatus::ACTIVE),
    ))
});

crate::sql_lazy! {
    PENDING_ORPHAN_FRONTIER_QUERY = || PENDING_ORPHAN_FRONTIER.as_str(),
        params = [],
        tier = Sweep;
}

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

pub async fn pending_orphan_frontier<C: ConnectionTrait>(db: &C) -> Result<bool, DbErr> {
    Ok(db
        .query_one_raw(PENDING_ORPHAN_FRONTIER_QUERY.stmt())
        .await?
        .is_some())
}

/// The conflict clause is absorbing a concurrent record naming the same pair.
pub async fn adopt_pending_closures<C>(db: &C) -> Result<Adopted, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    adopt(db, &ADOPT_LIVE_QUERY, []).await
}

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
    let walk = crate::graph::walks::begin_walk(db, query).await?;
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
    use crate::graph::walks::WALK_WORK_MEM;
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

    #[tokio::test]
    async fn adoption_names_every_open_shared_build_a_live_evaluation_reaches() {
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
                 AND NOT db.cache_available AND db.status IN (0, 1, 2, 8)) FROM build_job bj"
            ),
            "{sql}"
        );
        assert!(
            sql.contains(&format!(
                "WHERE EXISTS (SELECT 1 FROM evaluation ev WHERE ev.id = bj.evaluation \
                 AND ev.status IN ({})) AND (NOT db.fetchable AND db.status NOT IN (4, 6, 9))",
                crate::sql::status::eval_in(&EvaluationStatus::ACTIVE)
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
                 AND ((w.walked AND p.probed AND NOT p.cache_available \
                 AND p.status IN (0, 1, 2, 8)) OR e.kind IN (1, 2)) AND ev.status IN ("
            ),
            "{frontier}"
        );
    }
}
