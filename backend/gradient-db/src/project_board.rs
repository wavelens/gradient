/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Aggregate queries powering the project page: per-evaluation build-status and
//! message rollups, the live queue summary, and per-entry-point dependency-
//! closure counts.

use crate::fetch_in_chunks;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_entity::ids::{EntryPointId, EvaluationId, ProjectId};
use sea_orm::{ActiveEnum, ConnectionTrait, DbBackend, DbErr, FromQueryResult, Statement};
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
        let ids = chunk
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT bj.evaluation AS evaluation, db.status AS status, COUNT(*) AS cnt \
             FROM build_job bj JOIN derivation_build db ON db.id = bj.derivation_build \
             WHERE bj.evaluation IN ({ids}) \
             GROUP BY bj.evaluation, db.status"
        );
        EvalStatusCountRow::find_by_statement(Statement::from_string(DbBackend::Postgres, sql))
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
        let ids = chunk
            .iter()
            .map(|id| format!("'{id}'"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "SELECT evaluation, level, COUNT(*) AS cnt \
             FROM evaluation_message WHERE evaluation IN ({ids}) \
             GROUP BY evaluation, level"
        );
        EvalLevelCountRow::find_by_statement(Statement::from_string(DbBackend::Postgres, sql))
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

/// Live `building` / `queued` build counts across the project's non-finished
/// evaluations. Powers the "N building · M queued" header chip.
pub async fn project_queue_summary<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<(i64, i64), DbErr> {
    let sql = format!(
        "SELECT b.status AS status, COUNT(*) AS cnt \
         FROM build_job bj \
         JOIN evaluation e ON e.id = bj.evaluation \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         WHERE e.project = $1 AND e.status NOT IN ({eval_terminal}) \
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
        [project.into_inner().into()],
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

/// For each `(entry_point, root derivation)` seed, count this evaluation's
/// builds whose derivation lies in the entry point's build-time dependency
/// closure, excluding the entry point's own build. The recursive walk is pruned
/// to derivations that have a build in this evaluation, so it stays bounded by
/// the evaluation's build graph rather than the full Nix closure. Returns
/// `entry_point -> status -> count`.
pub async fn entry_point_dep_counts<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    seeds: &[(EntryPointId, Uuid)],
) -> Result<HashMap<EntryPointId, HashMap<BuildStatus, i64>>, DbErr> {
    if seeds.is_empty() {
        return Ok(HashMap::new());
    }

    let values = seeds
        .iter()
        .map(|(ep, drv)| format!("('{ep}'::uuid,'{drv}'::uuid)"))
        .collect::<Vec<_>>()
        .join(",");
    let eval = evaluation.into_inner();
    let sql = format!(
        "WITH RECURSIVE seeds(ep, root_drv) AS (VALUES {values}), \
         closure(ep, drv) AS ( \
            SELECT ep, root_drv FROM seeds \
            UNION \
            SELECT c.ep, dd.dependency \
            FROM closure c \
            JOIN derivation_dependency dd ON dd.derivation = c.drv \
            JOIN build_job bj ON bj.derivation = dd.dependency AND bj.evaluation = '{eval}' \
         ) \
         SELECT c.ep AS entry_point, b.status AS status, COUNT(*) AS cnt \
         FROM closure c \
         JOIN seeds s ON s.ep = c.ep \
         JOIN build_job bj ON bj.derivation = c.drv AND bj.evaluation = '{eval}' \
         JOIN derivation_build b ON b.id = bj.derivation_build \
         WHERE c.drv <> s.root_drv \
         GROUP BY c.ep, b.status"
    );

    let rows = DepCountRow::find_by_statement(Statement::from_string(DbBackend::Postgres, sql))
        .all(db)
        .await?;

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
