/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Aggregate queries powering the task page: per-evaluation build-status and
//! message rollups, the live queue summary, and per-entry-point dependency-
//! closure counts.

use crate::fetch_in_chunks;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{EntryPointId, EvaluationId, TaskId};
use sea_orm::{
    ActiveEnum, ConnectionTrait, DatabaseTransaction, DbBackend, DbErr, FromQueryResult, Statement,
    TransactionTrait,
};
use std::collections::HashMap;
use uuid::Uuid;

#[derive(Debug, FromQueryResult)]
struct EvalStatusCountRow {
    evaluation: Uuid,
    status: i32,
    cnt: i64,
}

/// `(evaluation, build.status) -> count`, one grouped query per chunk of ids.
pub async fn build_status_counts_by_evaluation<C: ConnectionTrait>(
    db: &C,
    eval_ids: &[EvaluationId],
) -> Result<HashMap<EvaluationId, HashMap<BuildStatus, i64>>, DbErr> {
    let rows = fetch_in_chunks(eval_ids, |chunk| async move {
        let ids: Vec<Uuid> = chunk.iter().map(|id| id.into_inner()).collect();
        EvalStatusCountRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT bj.evaluation AS evaluation, db.status AS status, COUNT(*) AS cnt \
             FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build \
             WHERE bj.evaluation = ANY($1) \
             GROUP BY bj.evaluation, db.status",
            [ids.into()],
        ))
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

/// `(evaluation, message.level) -> count`.
pub async fn evaluation_message_counts<C: ConnectionTrait>(
    db: &C,
    eval_ids: &[EvaluationId],
) -> Result<HashMap<EvaluationId, HashMap<MessageLevel, i64>>, DbErr> {
    let rows = fetch_in_chunks(eval_ids, |chunk| async move {
        let ids: Vec<Uuid> = chunk.iter().map(|id| id.into_inner()).collect();
        EvalLevelCountRow::find_by_statement(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT evaluation, level, COUNT(*) AS cnt \
             FROM evaluation_message WHERE evaluation = ANY($1) \
             GROUP BY evaluation, level",
            [ids.into()],
        ))
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

/// Live `building` / `queued` build counts across the task's non-finished
/// evaluations. Powers the "N building · M queued" header chip.
pub async fn task_queue_summary<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<(i64, i64), DbErr> {
    let sql = format!(
        "SELECT b.status AS status, COUNT(*) AS cnt \
         FROM build_job bj \
         JOIN evaluation e ON e.id = bj.evaluation \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         WHERE e.task = $1 AND e.status NOT IN ({eval_terminal}) \
           AND b.status IN ({live}) \
         GROUP BY b.status",
        eval_terminal =
            crate::status_sql::eval_in(&gradient_entity::evaluation::EvaluationStatus::TERMINAL),
        live = crate::status_sql::build_in(&[
            BuildStatus::Created,
            BuildStatus::Queued,
            BuildStatus::Building,
            BuildStatus::FailedTransient,
        ]),
    );
    let rows = StatusCountRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        sql,
        [task.into_inner().into()],
    ))
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

/// The evaluation's edges are materialised ONCE and the per-entry-point walk
/// runs over that set, which is the whole performance story. The answer is
/// inherently one row per (entry point, derivation) pair, and entry points of
/// one flake share nearly all of their closure: 74 NixOS hosts reached 1,868
/// derivations each out of a 10,689-derivation union, so a walk that probed
/// `derivation_dependency` per pair re-read the same 881 MB index 138,000 times.
/// Hoisting the edges leaves the pairs to a hash join over a set that fits in
/// the raised `work_mem`. Measured on that evaluation: 97.6 s before, 3.8 s
/// after, identical output.
///
/// `MATERIALIZED` is load-bearing (it stops the planner inlining the edge set
/// into each reference), and so is the source-side `IN`: the frontier only ever
/// holds a seed root or a derivation with a build in this evaluation, so that is
/// exactly the set of rows whose outgoing edges the walk can ask for. The
/// `LATERAL ... OFFSET 0` fence the other walks need is deliberately absent -
/// it exists to stop a merge join against the whole edge table, and here the
/// recursive term joins a small materialised set where the hash join is right.
const DEP_COUNTS_SQL: &str = "WITH RECURSIVE seeds(ep, root_drv) AS (SELECT * FROM unnest($1::uuid[], $2::uuid[])), \
    edges AS MATERIALIZED ( \
       SELECT dd.derivation, dd.dependency FROM derivation_dependency dd \
       JOIN build_job dst ON dst.derivation = dd.dependency AND dst.evaluation = $3 \
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

/// For each `(entry_point, root derivation)` seed, count this evaluation's
/// builds whose derivation lies in the entry point's build-time dependency
/// closure, excluding the entry point's own build. The walk is pruned to
/// derivations that have a build in this evaluation, so it stays bounded by the
/// evaluation's build graph rather than the full Nix closure. Returns
/// `entry_point -> status -> count`.
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
    let walk = crate::graph_sql::begin_walk(db).await?;
    let rows = DepCountRow::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres,
        DEP_COUNTS_SQL,
        [eps.into(), drvs.into(), evaluation.into_inner().into()],
    ))
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

    /// The answer is one row per (entry point, derivation) pair and the entry
    /// points of one flake share nearly all of their closure, so the recursive
    /// term must join a set hoisted out of the walk, never probe
    /// `derivation_dependency` again per pair. Inlining the edge set restores
    /// the 97.6 s shape this replaced.
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

    /// The frontier only ever holds a seed root or a derivation with a build in
    /// this evaluation. Widening the source side past those two would materialise
    /// edges of the whole global graph; narrowing it to build jobs alone would
    /// drop the outgoing edges of an entry point that has none.
    #[test]
    fn the_edge_set_covers_exactly_what_the_frontier_can_ask_for() {
        let sql = norm(DEP_COUNTS_SQL);

        assert!(
            sql.contains(
                "WHERE dd.derivation IN (SELECT derivation FROM build_job WHERE evaluation = $3 \
                 UNION SELECT root_drv FROM seeds)"
            ),
            "{sql}"
        );
        assert!(
            sql.contains(
                "JOIN build_job dst ON dst.derivation = dd.dependency AND dst.evaluation = $3"
            ),
            "the walk stays inside this evaluation's build graph: {sql}"
        );
    }

    /// An entry point's own build is not one of its dependencies, and the page
    /// reports the count per status.
    #[test]
    fn the_count_excludes_the_entry_points_own_build() {
        let sql = norm(DEP_COUNTS_SQL);

        assert!(sql.contains("WHERE c.drv <> s.root_drv"), "{sql}");
        assert!(sql.contains("GROUP BY c.ep, b.status"), "{sql}");
    }
}
