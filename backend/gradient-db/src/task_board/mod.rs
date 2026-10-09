/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod dep_counts;

use crate::fetch_in_chunks;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{EntryPointId, EvaluationId, TaskId};
use sea_orm::{
    ActiveEnum, ConnectionTrait, DatabaseTransaction, DbErr, FromQueryResult, TransactionTrait,
};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, FromQueryResult)]
struct EvalStatusCountRow {
    evaluation: Uuid,
    status: i32,
    cnt: i64,
}

crate::sql! {
    BUILD_STATUS_COUNTS_BY_EVALUATION = "SELECT bj.evaluation AS evaluation, db.status AS status, COUNT(*) AS cnt \
         FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build \
         WHERE bj.evaluation = ANY($1) \
         GROUP BY bj.evaluation, db.status",
        params = [EvaluationIds(64)],
        tier = Bulk;
}

pub async fn build_status_counts_by_evaluation<C: ConnectionTrait>(
    db: &C,
    eval_ids: &[EvaluationId],
) -> Result<HashMap<EvaluationId, HashMap<BuildStatus, i64>>, DbErr> {
    let rows = fetch_in_chunks(eval_ids, |chunk| async move {
        let ids: Vec<Uuid> = chunk.iter().map(|id| id.into_inner()).collect();
        EvalStatusCountRow::find_by_statement(BUILD_STATUS_COUNTS_BY_EVALUATION.bind([ids.into()]))
            .all(db)
            .await
    })
    .await?;

    let mut out: HashMap<EvaluationId, HashMap<BuildStatus, i64>> = HashMap::new();
    for r in rows {
        if let Ok(status) = BuildStatus::try_from(r.status) {
            *out.entry(EvaluationId(r.evaluation))
                .or_default()
                .entry(status)
                .or_insert(0) += r.cnt;
        }
    }

    Ok(out)
}

#[derive(Debug, FromQueryResult)]
struct EvalLevelCountRow {
    evaluation: Uuid,
    level: i32,
    cnt: i64,
}

crate::sql! {
    EVALUATION_MESSAGE_COUNTS = "SELECT evaluation, level, COUNT(*) AS cnt \
         FROM evaluation_message WHERE evaluation = ANY($1) \
         GROUP BY evaluation, level",
        params = [EvaluationIds(64)];
}

pub async fn evaluation_message_counts<C: ConnectionTrait>(
    db: &C,
    eval_ids: &[EvaluationId],
) -> Result<HashMap<EvaluationId, HashMap<MessageLevel, i64>>, DbErr> {
    let rows = fetch_in_chunks(eval_ids, |chunk| async move {
        let ids: Vec<Uuid> = chunk.iter().map(|id| id.into_inner()).collect();
        EvalLevelCountRow::find_by_statement(EVALUATION_MESSAGE_COUNTS.bind([ids.into()]))
            .all(db)
            .await
    })
    .await?;

    let mut out: HashMap<EvaluationId, HashMap<MessageLevel, i64>> = HashMap::new();
    for r in rows {
        if let Ok(level) = MessageLevel::try_from_value(&r.level) {
            *out.entry(EvaluationId(r.evaluation))
                .or_default()
                .entry(level)
                .or_insert(0) += r.cnt;
        }
    }

    Ok(out)
}

#[derive(Debug, FromQueryResult)]
struct StatusCountRow {
    status: i32,
    cnt: i64,
}

fn task_queue_summary_sql() -> String {
    format!(
        "SELECT b.status AS status, COUNT(*) AS cnt \
         FROM build_job bj \
         JOIN evaluation e ON e.id = bj.evaluation \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         WHERE e.task = $1 AND e.status NOT IN ({eval_terminal}) \
           AND b.status IN ({live}) \
         GROUP BY b.status",
        eval_terminal =
            crate::sql::status::eval_in(&gradient_entity::evaluation::EvaluationStatus::TERMINAL),
        live = crate::sql::status::build_in(&[
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
            BuildStatus::FailedTransient,
        ]),
    )
}

crate::sql_fn! {
    TASK_QUEUE_SUMMARY = task_queue_summary_sql,
        params = [TaskId],
        tier = Bulk;
}

pub async fn task_queue_summary<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<(i64, i64), DbErr> {
    let rows =
        StatusCountRow::find_by_statement(TASK_QUEUE_SUMMARY.bind([task.into_inner().into()]))
            .all(db)
            .await?;

    let mut building = 0i64;
    let mut queued = 0i64;
    for r in rows {
        match BuildStatus::try_from(r.status) {
            Ok(BuildStatus::Building) => building += r.cnt,
            Ok(BuildStatus::Created | BuildStatus::Queued | BuildStatus::FailedTransient) => {
                queued += r.cnt
            }
            _ => {}
        }
    }

    Ok((building, queued))
}

#[derive(Debug, FromQueryResult)]
struct DepCountRow {
    entry_point: Uuid,
    status: i32,
    cnt: i64,
}

/// `MATERIALIZED` is stopping the planner from inlining the edge set. The edge set reads the
/// evaluation's jobs only once: a new evaluation is missing from the statistics, and a second read
/// of its jobs planned as a nested loop over both reads.
const DEP_COUNTS_SQL: &str = "WITH RECURSIVE seeds(ep, root_drv) AS (SELECT * FROM unnest($1::uuid[], $2::uuid[])), \
    edges AS MATERIALIZED ( \
       SELECT dd.derivation, dd.dependency FROM derivation_dependency dd \
       WHERE dd.derivation IN (SELECT derivation FROM build_job WHERE evaluation = $3 \
                               UNION SELECT root_drv FROM seeds) \
    ), \
    closure(ep, drv) AS ( \
       SELECT ep, root_drv FROM seeds \
       UNION \
       SELECT c.ep, e.dependency FROM closure c JOIN edges e ON e.derivation = c.drv \
    ) \
    SELECT c.ep AS entry_point, b.status AS status, COUNT(*) AS cnt \
    FROM closure c \
    JOIN seeds s ON s.ep = c.ep \
    JOIN build_job bj ON bj.derivation = c.drv AND bj.evaluation = $3 \
    JOIN derivation_build b ON b.id = bj.derivation_build \
    WHERE c.drv <> s.root_drv \
    GROUP BY c.ep, b.status";

crate::sql! {
    DEP_COUNTS_SQL_QUERY = DEP_COUNTS_SQL,
        params = [
            EntryPointIds(64),
            DerivationIds(64),
            EvaluationId,
        ],
        tier = Bulk,
        budget = crate::sql::Budget::bulk().buffers(100_000)
            .because("counts the closure of 64 entry points inside the fixture's largest \
                      evaluation, ~98k jobs"),
        flags = [Walk];
}

pub async fn entry_point_dep_counts<C>(
    db: &C,
    evaluation: EvaluationId,
    seeds: &[(EntryPointId, Uuid)],
) -> Result<HashMap<EntryPointId, HashMap<BuildStatus, i64>>, DbErr>
where
    C: ConnectionTrait + TransactionTrait<Transaction = DatabaseTransaction>,
{
    if seeds.is_empty() {
        return Ok(HashMap::new());
    }

    let (eps, drvs): (Vec<Uuid>, Vec<Uuid>) = seeds
        .iter()
        .map(|(ep, drv)| (ep.into_inner(), *drv))
        .unzip();
    let walk = crate::graph::walks::begin_walk(db).await?;
    let rows = DepCountRow::find_by_statement(DEP_COUNTS_SQL_QUERY.bind([
        eps.into(),
        drvs.into(),
        evaluation.into_inner().into(),
    ]))
    .all(&walk)
    .await?;
    walk.commit().await?;

    let mut out: HashMap<EntryPointId, HashMap<BuildStatus, i64>> = HashMap::new();
    for r in rows {
        if let Ok(status) = BuildStatus::try_from(r.status) {
            *out.entry(EntryPointId(r.entry_point))
                .or_default()
                .entry(status)
                .or_insert(0) += r.cnt;
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::DEP_COUNTS_SQL;

    fn norm(s: &str) -> String {
        s.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    #[test]
    fn the_walk_joins_a_materialised_edge_set_once() {
        let sql = norm(DEP_COUNTS_SQL);

        assert!(sql.contains("edges AS MATERIALIZED ("), "{sql}");
        assert!(
            sql.contains(
                "SELECT c.ep, e.dependency FROM closure c JOIN edges e ON e.derivation = c.drv"
            ),
            "the recursive term must read the hoisted edges: {sql}"
        );

        let recursive = sql.split("closure(ep, drv) AS (").nth(1).expect("the walk");
        assert!(
            !recursive.contains("derivation_dependency"),
            "the recursive term probes the edge table per pair: {recursive}"
        );
    }

    #[test]
    fn the_edge_set_reads_the_evaluations_jobs_only_once() {
        let sql = norm(DEP_COUNTS_SQL);
        let edges = sql
            .split("closure(ep, drv) AS (")
            .next()
            .expect("the edge set");

        assert_eq!(edges.matches("build_job").count(), 1, "{edges}");
    }
}
