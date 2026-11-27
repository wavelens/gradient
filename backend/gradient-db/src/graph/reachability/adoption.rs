/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::graph::{
    predicates::{builder_predicate, open_predicate},
    walks::{open_closure_cte, open_closure_cte_until},
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
        "{cte} {insert}",
        cte = open_closure_cte("pending", seed_select),
        insert = adopt_insert(),
    )
}

fn adopt_insert() -> &'static str {
    "INSERT INTO build_job \
     (id, evaluation, derivation, derivation_build, score, score_breakdown, created_at) \
     SELECT uuidv7(), p.evaluation, p.derivation, db.id, 0, '{}'::jsonb, \
     (now() AT TIME ZONE 'UTC') \
     FROM pending p JOIN derivation_build db ON db.derivation = p.derivation \
     ON CONFLICT (evaluation, derivation) DO NOTHING \
     RETURNING evaluation, derivation"
}

static ADOPT_LIVE: LazyLock<String> = LazyLock::new(|| {
    adopt_sql(&named_open(&format!(
        "EXISTS (SELECT 1 FROM evaluation ev WHERE ev.id = bj.evaluation AND ev.status IN ({live}))",
        live = crate::sql::status::eval_in(&EvaluationStatus::ACTIVE),
    )))
});

const ADOPTION_WALK_STOP_WHEN_EVALUATION_ALREADY_NAMES_DEPENDENCY_SQL: &str = "EXISTS (SELECT 1 FROM build_job x WHERE x.evaluation = c.evaluation AND x.derivation = e.dependency)";

static ADOPT_EVAL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{cte} {insert}",
        cte = open_closure_cte_until(
            "pending",
            &named_open("bj.evaluation = $1"),
            ADOPTION_WALK_STOP_WHEN_EVALUATION_ALREADY_NAMES_DEPENDENCY_SQL,
        ),
        insert = adopt_insert(),
    )
});

fn below_named_parents(parent_build_is_wanted: bool) -> String {
    format!(
        "SELECT parent.evaluation, db.derivation, ({builder}) \
         FROM unnest($1::uuid[]) AS g(derivation) \
         JOIN derivation_build db ON db.derivation = g.derivation \
         JOIN derivation w ON w.id = db.derivation \
         JOIN derivation_dependency e ON e.dependency = db.derivation \
         CROSS JOIN LATERAL (SELECT pj.evaluation FROM derivation_build p \
                             JOIN derivation pw ON pw.id = p.derivation \
                             JOIN build_job pj ON pj.derivation = p.derivation \
                             JOIN evaluation ev ON ev.id = pj.evaluation \
                             WHERE p.derivation = e.derivation{wanted} AND {parent_open} \
                               AND (({parent_builder}) OR e.kind IN (1, 2)) \
                               AND ev.status IN ({live}) OFFSET 0) parent \
         WHERE {open}",
        builder = builder_predicate("db", "w"),
        open = open_predicate("db"),
        wanted = if parent_build_is_wanted {
            " AND p.wanted"
        } else {
            ""
        },
        parent_open = open_predicate("p"),
        parent_builder = builder_predicate("p", "pw"),
        live = crate::sql::status::eval_in(&EvaluationStatus::ACTIVE),
    )
}

static ADOPT_BELOW_NAMED: LazyLock<String> =
    LazyLock::new(|| adopt_sql(&below_named_parents(true)));

static INSERT_BUILD_JOBS_BELOW_LIVE_NAMED_PARENTS_IGNORING_WANTED_SQL: LazyLock<String> =
    LazyLock::new(|| adopt_sql(&below_named_parents(false)));

static ADOPT_UNNAMED_BELOW: LazyLock<String> = LazyLock::new(|| {
    let named_roots = named_open(&format!(
        "bj.derivation = ANY($1::uuid[]) AND EXISTS (SELECT 1 FROM evaluation ev \
         WHERE ev.id = bj.evaluation AND ev.status IN ({live}))",
        live = crate::sql::status::eval_in(&EvaluationStatus::ACTIVE),
    ));
    format!(
        "{cte} {insert}",
        cte = open_closure_cte_until(
            "pending",
            &format!("{named_roots} UNION {}", below_named_parents(true)),
            ADOPTION_WALK_STOP_WHEN_EVALUATION_ALREADY_NAMES_DEPENDENCY_SQL,
        ),
        insert = adopt_insert(),
    )
});

crate::sql_lazy! {
    ADOPT_BELOW_NAMED_QUERY = || ADOPT_BELOW_NAMED.as_str(),
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

crate::sql_lazy! {
    INSERT_BUILD_JOBS_BELOW_LIVE_NAMED_PARENTS_IGNORING_WANTED_QUERY =
        || INSERT_BUILD_JOBS_BELOW_LIVE_NAMED_PARENTS_IGNORING_WANTED_SQL.as_str(),
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

crate::sql_lazy! {
    ADOPT_UNNAMED_BELOW_QUERY = || ADOPT_UNNAMED_BELOW.as_str(),
        params = [DerivationIds(64)],
        tier = Walk,
        flags = [Walk];
}

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

pub const BUILD_SCAN_BATCH_SIZE: usize = 20_000;

static OPEN_BUILDS_WITHOUT_BUILD_JOB_WINDOW_SCAN: LazyLock<String> = LazyLock::new(|| {
    format!(
        "WITH scanned AS MATERIALIZED (SELECT db.derivation FROM derivation_build db \
                                      WHERE db.derivation > $1 AND {open} \
                                      ORDER BY db.derivation LIMIT $2), \
         without_build_job AS (SELECT s.derivation FROM scanned s WHERE NOT EXISTS \
                               (SELECT 1 FROM build_job bj WHERE bj.derivation = s.derivation)), \
         window_end AS (SELECT s.derivation, (SELECT count(*) FROM scanned) AS scanned_count \
                        FROM scanned s ORDER BY s.derivation DESC LIMIT 1) \
         SELECT e.derivation AS last_derivation_id, \
                e.scanned_count AS open_build_count_in_window, \
                u.derivation AS derivation_id_without_build_job \
         FROM window_end e LEFT JOIN without_build_job u ON true",
        open = open_predicate("db"),
    )
});

crate::sql_lazy! {
    OPEN_BUILDS_WITHOUT_BUILD_JOB_WINDOW_SCAN_QUERY =
        || OPEN_BUILDS_WITHOUT_BUILD_JOB_WINDOW_SCAN.as_str(),
        params = [DerivationId, Int(BUILD_SCAN_BATCH_SIZE as i64)],
        tier = Sweep;
}

#[derive(Debug, Default, PartialEq)]
pub struct OpenBuildWindowScanResult {
    pub derivation_ids_without_build_job: Vec<DerivationId>,
    pub last_derivation_id: Option<DerivationId>,
    pub open_build_count_in_window: usize,
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

pub async fn read_open_build_window_without_build_job<C: ConnectionTrait>(
    db: &C,
    after: DerivationId,
    open_builds_scanned_per_transaction: usize,
) -> Result<OpenBuildWindowScanResult, DbErr> {
    let rows = db
        .query_all_raw(OPEN_BUILDS_WITHOUT_BUILD_JOB_WINDOW_SCAN_QUERY.bind([
            Value::Uuid(Some(after.into_inner())),
            Value::BigInt(Some(open_builds_scanned_per_transaction as i64)),
        ]))
        .await?;
    let Some(first) = rows.first() else {
        return Ok(OpenBuildWindowScanResult::default());
    };

    Ok(OpenBuildWindowScanResult {
        derivation_ids_without_build_job: rows
            .iter()
            .filter_map(|r| {
                r.try_get::<Option<uuid::Uuid>>("", "derivation_id_without_build_job")
                    .map(|d| d.map(DerivationId::new))
                    .transpose()
            })
            .collect::<Result<_, DbErr>>()?,
        last_derivation_id: Some(DerivationId::new(
            first.try_get::<uuid::Uuid>("", "last_derivation_id")?,
        )),
        open_build_count_in_window: usize::try_from(
            first.try_get::<i64>("", "open_build_count_in_window")?,
        )
        .unwrap_or(usize::MAX),
    })
}

/// The conflict clause is absorbing a concurrent record naming the same pair.
pub async fn adopt_pending_closures<C>(db: &C) -> Result<Adopted, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    adopt(db, &ADOPT_LIVE_QUERY, []).await
}

#[tracing::instrument(level = "debug", skip_all)]
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

pub async fn adopt_below_named<C>(db: &C, derivations: &[DerivationId]) -> Result<Adopted, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if derivations.is_empty() {
        return Ok(Adopted::default());
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    adopt(db, &ADOPT_BELOW_NAMED_QUERY, [ids.into()]).await
}

pub async fn adopt_unnamed_below_for_live_evaluations<C>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<Adopted, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if derivations.is_empty() {
        return Ok(Adopted::default());
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    adopt(
        db,
        &INSERT_BUILD_JOBS_BELOW_LIVE_NAMED_PARENTS_IGNORING_WANTED_QUERY,
        [ids.into()],
    )
    .await
}

pub async fn adopt_unnamed_below<C>(db: &C, derivations: &[DerivationId]) -> Result<Adopted, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if derivations.is_empty() {
        return Ok(Adopted::default());
    }

    let ids: Vec<uuid::Uuid> = derivations.iter().map(|d| d.into_inner()).collect();
    adopt(db, &ADOPT_UNNAMED_BELOW_QUERY, [ids.into()]).await
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
    use gradient_entity::build::BuildStatus;

    fn path(key: &str) -> String {
        format!("/nix/store/{}-adopt.drv", key.repeat(32))
    }

    async fn sorted_pairs(
        db: &gradient_fixtures::database::TestDatabase,
        expected: &[(EvaluationId, &str)],
    ) -> Vec<(EvaluationId, DerivationId)> {
        let mut pairs = Vec::new();
        for (evaluation, key) in expected {
            pairs.push((*evaluation, db.id_of(&path(key)).await));
        }
        pairs.sort_unstable();

        pairs
    }

    fn sorted(adopted: Adopted) -> Vec<(EvaluationId, DerivationId)> {
        let mut pairs = adopted.pairs;
        pairs.sort_unstable();

        pairs
    }

    #[test]
    fn adopted_evaluations_and_derivations_are_sorted_and_deduplicated() {
        let (first, second) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let (a, b) = (DerivationId::now_v7(), DerivationId::now_v7());
        let adopted = Adopted {
            pairs: vec![(second, b), (first, b), (second, a), (first, a)],
        };

        assert_eq!(adopted.evaluations(), vec![first, second]);
        assert_eq!(adopted.derivations(), vec![a, b]);
    }

    #[tokio::test]
    async fn adopt_pending_closures_names_open_builds_and_skips_finished_evaluations() {
        let [named, unnamed, deeper_named, deeper_unnamed, closed, failed] =
            ["1", "2", "3", "4", "5", "6"];
        let [live_root, live_below, done_root, done_below] = ["7", "8", "9", "a"];
        let (first, second, finished) = (
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
        );
        let db = gradient_fixtures::shape::shape()
            .evaluation(first)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(&path(named))
            .walked()
            .probed()
            .job()
            .depends_on(&path(unnamed))
            .depends_on(&path(closed))
            .derivation(&path(unnamed))
            .walked()
            .probed()
            .depends_on(&path(deeper_named))
            .depends_on(&path(failed))
            .derivation(&path(deeper_named))
            .walked()
            .probed()
            .job()
            .depends_on(&path(deeper_unnamed))
            .derivation(&path(deeper_unnamed))
            .derivation(&path(closed))
            .output(&closed.repeat(32))
            .build_status(BuildStatus::Completed)
            .cached_path(&closed.repeat(32), "closed")
            .derivation(&path(failed))
            .build_status(BuildStatus::FailedPermanent)
            .evaluation(second)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(&path(live_root))
            .walked()
            .probed()
            .job()
            .depends_on(&path(live_below))
            .derivation(&path(live_below))
            .evaluation(finished)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(&path(done_root))
            .walked()
            .probed()
            .job()
            .depends_on(&path(done_below))
            .derivation(&path(done_below))
            .into_database()
            .await;
        db.execute("UPDATE derivation_build SET fetchable = (status = 3)")
            .await;
        let connection = db.connection();

        let one_evaluation = adopt_pending_closure(&connection, first).await.unwrap();
        let live = adopt_pending_closures(&connection).await.unwrap();
        let again = adopt_pending_closures(&connection).await.unwrap();
        let expected_one = sorted_pairs(&db, &[(first, unnamed), (first, deeper_unnamed)]).await;
        let expected_live = sorted_pairs(&db, &[(second, live_below)]).await;
        let (one_evaluation, live) = (sorted(one_evaluation), sorted(live));
        db.finish().await;

        assert_eq!(one_evaluation, expected_one);
        assert_eq!(live, expected_live);
        assert!(again.pairs.is_empty());
    }

    #[tokio::test]
    async fn test_adoption_below_rules_for_named_and_unnamed_parents() {
        let [wanted_parent, below_wanted, unwanted_parent, below_unwanted] = ["1", "2", "3", "4"];
        let [
            named_root,
            below_named_root,
            finished_root,
            below_finished_root,
        ] = ["5", "6", "7", "8"];
        let (live, finished) = (EvaluationId::now_v7(), EvaluationId::now_v7());
        let db = gradient_fixtures::shape::shape()
            .evaluation(live)
            .evaluation_status(EvaluationStatus::Building)
            .derivation(&path(wanted_parent))
            .walked()
            .probed()
            .job()
            .depends_on(&path(below_wanted))
            .derivation(&path(below_wanted))
            .derivation(&path(unwanted_parent))
            .walked()
            .probed()
            .job()
            .depends_on(&path(below_unwanted))
            .derivation(&path(below_unwanted))
            .derivation(&path(named_root))
            .walked()
            .probed()
            .job()
            .depends_on(&path(below_named_root))
            .derivation(&path(below_named_root))
            .evaluation(finished)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(&path(finished_root))
            .walked()
            .probed()
            .job()
            .depends_on(&path(below_finished_root))
            .derivation(&path(below_finished_root))
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE derivation_build SET wanted = true; \
             UPDATE derivation_build SET wanted = false WHERE derivation = '{}'",
            db.id_of(&path(unwanted_parent)).await
        ))
        .await;
        let connection = db.connection();
        let below_both = [
            db.id_of(&path(below_wanted)).await,
            db.id_of(&path(below_unwanted)).await,
        ];
        let roots = [
            db.id_of(&path(named_root)).await,
            db.id_of(&path(finished_root)).await,
        ];

        let below_wanted_parents = adopt_below_named(&connection, &below_both).await.unwrap();
        let ignoring_wanted = adopt_unnamed_below_for_live_evaluations(&connection, &below_both)
            .await
            .unwrap();
        let below_roots = adopt_unnamed_below(&connection, &roots).await.unwrap();
        let expected_wanted = sorted_pairs(&db, &[(live, below_wanted)]).await;
        let expected_ignoring = sorted_pairs(&db, &[(live, below_unwanted)]).await;
        let expected_roots = sorted_pairs(&db, &[(live, below_named_root)]).await;
        let results = (
            sorted(below_wanted_parents),
            sorted(ignoring_wanted),
            sorted(below_roots),
        );
        db.finish().await;

        assert_eq!(
            results,
            (expected_wanted, expected_ignoring, expected_roots)
        );
    }

    #[tokio::test]
    async fn test_open_builds_without_build_job_are_reported_and_closed_or_out_of_scope_builds_are_ignored()
     {
        let [orphan, failed, named, fetchable, outside_scope] = ["1", "2", "3", "4", "5"];
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&path(orphan))
            .derivation(&path(failed))
            .build_status(BuildStatus::FailedPermanent)
            .derivation(&path(named))
            .job()
            .derivation(&path(fetchable))
            .output(&fetchable.repeat(32))
            .build_status(BuildStatus::Completed)
            .cached_path(&fetchable.repeat(32), "fetchable")
            .derivation(&path(outside_scope))
            .into_database()
            .await;
        db.execute("UPDATE derivation_build SET fetchable = (status = 3)")
            .await;
        let connection = db.connection();
        let mut ids = Vec::new();
        for key in [orphan, failed, named, fetchable, outside_scope] {
            ids.push(db.id_of(&path(key)).await);
        }

        let with_orphan = pending_orphans_among(&connection, &ids[..4]).await.unwrap();
        let without_orphan = pending_orphans_among(&connection, &ids[1..4])
            .await
            .unwrap();
        let empty_scope = pending_orphans_among(&connection, &[]).await.unwrap();
        let nil = DerivationId::new(uuid::Uuid::nil());
        let whole = read_open_build_window_without_build_job(&connection, nil, 100)
            .await
            .unwrap();
        let mut open = [ids[0], ids[2], ids[4]];
        open.sort_unstable();
        let first_two = read_open_build_window_without_build_job(&connection, nil, 2)
            .await
            .unwrap();
        let after_all = read_open_build_window_without_build_job(&connection, open[2], 100)
            .await
            .unwrap();
        db.finish().await;

        assert_eq!(
            (with_orphan, without_orphan, empty_scope),
            (true, false, false)
        );
        let mut orphans = whole.derivation_ids_without_build_job.clone();
        orphans.sort_unstable();
        let mut expected_orphans = vec![ids[0], ids[4]];
        expected_orphans.sort_unstable();
        assert_eq!(orphans, expected_orphans);
        assert_eq!(
            (whole.open_build_count_in_window, whole.last_derivation_id),
            (3, Some(open[2]))
        );
        assert_eq!(
            (
                first_two.open_build_count_in_window,
                first_two.last_derivation_id
            ),
            (2, Some(open[1]))
        );
        assert_eq!(after_all, OpenBuildWindowScanResult::default());
    }
}
