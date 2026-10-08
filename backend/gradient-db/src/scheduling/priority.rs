/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::build_request_task::BUILD_REQUEST_TASK_NAME;
use crate::{DbContext, fetch_in_chunks, graph::closure::transitive_closure_reachable};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};
use std::collections::HashSet;

pub const PRIORITIZABLE: [BuildStatus; 4] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
];

#[derive(FromQueryResult)]
struct IdRow {
    id: uuid::Uuid,
}

fn prioritize_evaluation_sql() -> String {
    format!(
        "UPDATE evaluation SET prioritized = true \
         WHERE id = $1 AND status NOT IN ({}) RETURNING id",
        crate::sql::status::eval_in(&EvaluationStatus::TERMINAL)
    )
}

crate::sql_fn! {
    PRIORITIZE_EVALUATION = prioritize_evaluation_sql,
        params = [EvaluationId];
}

fn evaluation_open_shared_builds_sql() -> String {
    format!(
        "SELECT db.id AS id FROM build_job bj \
         JOIN derivation_build db ON db.id = bj.derivation_build \
         WHERE bj.evaluation = $1 AND db.status IN ({})",
        crate::sql::status::build_in(&PRIORITIZABLE)
    )
}

crate::sql_fn! {
    EVALUATION_OPEN_SHARED_BUILDS = evaluation_open_shared_builds_sql,
        params = [EvaluationId],
        tier = Bulk;
}

fn prioritize_shared_builds_sql() -> String {
    format!(
        "UPDATE derivation_build db SET prioritized = true \
         FROM unnest($1::uuid[]) AS x(derivation) \
         WHERE db.derivation = x.derivation AND db.status IN ({}) AND NOT db.prioritized \
         RETURNING db.id AS id",
        crate::sql::status::build_in(&PRIORITIZABLE)
    )
}

crate::sql_fn! {
    PRIORITIZE_SHARED_BUILDS = prioritize_shared_builds_sql,
        params = [DerivationIds(64)],
        tier = Bulk;
}

/// Mirrors the scheduler's QoS lift: a running evaluation that a user prioritized or that runs a
/// build request.
fn evaluation_has_qos_sql(evaluation: &str) -> String {
    format!(
        "{evaluation}.status NOT IN ({}) AND ({evaluation}.prioritized OR EXISTS (\
         SELECT 1 FROM task t WHERE t.id = {evaluation}.task AND t.managed AND t.name = '{}'))",
        crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
        BUILD_REQUEST_TASK_NAME,
    )
}

fn evaluations_with_qos_sql() -> String {
    format!(
        "SELECT e.id AS id FROM evaluation e WHERE e.id = ANY($1) AND {}",
        evaluation_has_qos_sql("e")
    )
}

crate::sql_fn! {
    EVALUATIONS_WITH_QOS = evaluations_with_qos_sql,
        params = [EvaluationIds(64)];
}

fn shared_builds_with_qos_sql() -> String {
    format!(
        "SELECT db.id AS id FROM derivation_build db \
         WHERE db.id = ANY($1) AND (db.prioritized OR EXISTS (\
         SELECT 1 FROM build_job bj JOIN evaluation e ON e.id = bj.evaluation \
         WHERE bj.derivation_build = db.id AND {}))",
        evaluation_has_qos_sql("e")
    )
}

crate::sql_fn! {
    SHARED_BUILDS_WITH_QOS = shared_builds_with_qos_sql,
        params = [SharedBuildIds(64)],
        tier = Bulk;
}

fn import_lifted_shared_builds_sql() -> String {
    let open = |alias| crate::graph::predicates::open_predicate(alias);
    let seed = format!(
        "SELECT root.derivation FROM derivation_build root \
         WHERE root.id = ANY($1::uuid[]) AND {}",
        open("root"),
    );
    let closure = crate::graph::walks::bounded_dependency_closure_cte_body(
        "imported",
        &seed,
        crate::graph::walks::ClosureDirection::Dependencies,
        &format!(
            "EXISTS (SELECT 1 FROM derivation_build dep \
             WHERE dep.derivation = e.dependency AND {})",
            open("dep")
        ),
        None,
    );
    format!(
        "WITH RECURSIVE {closure} \
         SELECT db.id AS id FROM derivation_build db \
         JOIN imported i ON i.derivation = db.derivation"
    )
}

crate::sql_fn! {
    IMPORT_LIFTED_SHARED_BUILDS = import_lifted_shared_builds_sql,
        params = [SharedBuildIds(64)],
        tier = Bulk;
}

pub async fn import_lifted_shared_builds<C: ConnectionTrait>(
    db: &C,
    waited: &[DerivationBuildId],
) -> Result<HashSet<DerivationBuildId>, DbErr> {
    let rows = fetch_in_chunks(waited, |chunk| async move {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|id| id.into_inner()).collect();
        IdRow::find_by_statement(IMPORT_LIFTED_SHARED_BUILDS.bind([ids.into()]))
            .all(db)
            .await
    })
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DerivationBuildId::from(r.id))
        .collect())
}

pub async fn prioritize_evaluation(
    ctx: &DbContext,
    evaluation: EvaluationId,
) -> Result<Vec<DerivationBuildId>, DbErr> {
    let id: sea_orm::Value = evaluation.into_inner().into();
    let flagged = ctx
        .worker_db
        .query_one_raw(PRIORITIZE_EVALUATION.bind([id.clone()]))
        .await?;
    if flagged.is_none() {
        return Ok(Vec::new());
    }

    let rows = IdRow::find_by_statement(EVALUATION_OPEN_SHARED_BUILDS.bind([id]))
        .all(&ctx.worker_db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| DerivationBuildId::from(r.id))
        .collect())
}

pub async fn prioritize_build_closure(
    ctx: &DbContext,
    shared_build: &MDerivationBuild,
) -> Result<Vec<DerivationBuildId>, DbErr> {
    let mut closure: Vec<uuid::Uuid> =
        transitive_closure_reachable(&ctx.worker_db, &[shared_build.derivation])
            .await?
            .into_iter()
            .map(|d| d.into_inner())
            .collect();
    closure.sort_unstable();

    let rows = IdRow::find_by_statement(PRIORITIZE_SHARED_BUILDS.bind([closure.into()]))
        .all(&ctx.worker_db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| DerivationBuildId::from(r.id))
        .collect())
}

pub async fn evaluations_with_qos<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<HashSet<EvaluationId>, DbErr> {
    let rows = fetch_in_chunks(evaluations, |chunk| async move {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|id| id.into_inner()).collect();
        IdRow::find_by_statement(EVALUATIONS_WITH_QOS.bind([ids.into()]))
            .all(db)
            .await
    })
    .await?;
    Ok(rows.into_iter().map(|r| EvaluationId::from(r.id)).collect())
}

pub async fn shared_builds_with_qos<C: ConnectionTrait>(
    db: &C,
    shared_builds: &[DerivationBuildId],
) -> Result<HashSet<DerivationBuildId>, DbErr> {
    let rows = fetch_in_chunks(shared_builds, |chunk| async move {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|id| id.into_inner()).collect();
        IdRow::find_by_statement(SHARED_BUILDS_WITH_QOS.bind([ids.into()]))
            .all(db)
            .await
    })
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| DerivationBuildId::from(r.id))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prioritizable_excludes_every_state_the_database_unprioritizes() {
        for status in [
            BuildStatus::FailedPermanent,
            BuildStatus::Aborted,
            BuildStatus::DependencyFailed,
            BuildStatus::FailedTimeout,
            BuildStatus::Skipped,
        ] {
            assert!(!PRIORITIZABLE.contains(&status), "{status:?}");
        }
    }

    #[test]
    fn shared_build_write_is_bounded_by_the_unnested_closure() {
        let sql = prioritize_shared_builds_sql();
        assert!(
            sql.contains("FROM unnest($1::uuid[]) AS x(derivation)"),
            "{sql}"
        );
        assert!(sql.contains("AND NOT db.prioritized"), "{sql}");
        assert!(sql.contains("status IN (0, 1, 2, 8)"), "{sql}");
    }

    #[test]
    fn the_import_lift_walks_the_open_closure_of_the_waited_builds_only() {
        let sql = import_lifted_shared_builds_sql();
        let (seed, step) = sql
            .split_once(" UNION ")
            .expect("the closure is a recursive union");
        assert!(
            seed.contains("WHERE root.id = ANY($1::uuid[]) AND (NOT root.fetchable"),
            "the walk starts at the builds an evaluation waits on: {sql}"
        );
        assert!(!sql.contains("ifd"), "{sql}");
        assert!(
            step.contains("dep.derivation = e.dependency AND (NOT dep.fetchable"),
            "a finished dependency ends the walk: {sql}"
        );
    }
}
