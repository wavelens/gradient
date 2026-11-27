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

    #[tokio::test]
    async fn test_prioritize_shared_builds_flags_only_open_unprioritized_shared_builds_and_returns_their_ids()
     {
        use gradient_fixtures::shape::shape;

        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-open.drv")
            .build_status(BuildStatus::Queued)
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-finished.drv")
            .build_status(BuildStatus::Completed)
            .derivation("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-flagged.drv")
            .build_status(BuildStatus::Queued)
            .into_database()
            .await;
        let conn = db.connection();
        let open = db
            .shared_build("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-open.drv")
            .await;
        let finished = db
            .shared_build("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-finished.drv")
            .await;
        let flagged = db
            .shared_build("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-flagged.drv")
            .await;
        db.execute(&format!(
            "UPDATE derivation_build SET prioritized = true WHERE id = '{}'",
            flagged.id.into_inner()
        ))
        .await;
        let derivations: Vec<uuid::Uuid> = [&open, &finished, &flagged]
            .iter()
            .map(|build| build.derivation.into_inner())
            .collect();

        let first =
            IdRow::find_by_statement(PRIORITIZE_SHARED_BUILDS.bind([derivations.clone().into()]))
                .all(&conn)
                .await
                .expect("flag the open shared builds");
        let again = IdRow::find_by_statement(PRIORITIZE_SHARED_BUILDS.bind([derivations.into()]))
            .all(&conn)
            .await
            .expect("flag the open shared builds again");
        let flagged_ids = db
            .texts("SELECT id::text FROM derivation_build WHERE prioritized ORDER BY id")
            .await;
        db.finish().await;

        assert_eq!(
            (
                first.iter().map(|row| row.id).collect::<Vec<_>>(),
                again.len(),
                flagged_ids.len()
            ),
            (vec![open.id.into_inner()], 0, 2)
        );
    }

    #[tokio::test]
    async fn import_lift_walks_open_dependencies_and_stops_at_finished_dependencies() {
        use gradient_fixtures::shape::shape;

        const WAITED: &str = "/nix/store/0123456789abcdfghijklmnpqrsvwxyz-waited.drv";
        const OPEN_IFD: &str = "/nix/store/0c7kxbq0fdq6pnxpzhg5yvbbrylx4v3f-open-ifd.drv";
        const FINISHED: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-finished.drv";
        const BEHIND: &str = "/nix/store/0c0f6hb8s2q2wbdrhl5ch9fwr8v2rq7x-behind.drv";
        const UNRELATED: &str = "/nix/store/dddddddddddddddddddddddddddddddd-unrelated.drv";
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(WAITED)
            .build_status(BuildStatus::Queued)
            .depends_on(OPEN_IFD)
            .derivation(OPEN_IFD)
            .ifd()
            .build_status(BuildStatus::Queued)
            .depends_on(FINISHED)
            .derivation(FINISHED)
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.fetchable = true)
            .depends_on(BEHIND)
            .derivation(BEHIND)
            .build_status(BuildStatus::Queued)
            .derivation(UNRELATED)
            .build_status(BuildStatus::Queued)
            .into_database()
            .await;
        let id = |path: &'static str| {
            let db = &db;
            async move { db.shared_build(path).await.id }
        };
        let (waited, open_ifd, unrelated) =
            (id(WAITED).await, id(OPEN_IFD).await, id(UNRELATED).await);

        let lifted = import_lifted_shared_builds(&db.connection(), &[waited])
            .await
            .expect("the import lift reads");

        db.finish().await;
        assert_eq!(lifted, HashSet::from([waited, open_ifd]));
        assert!(!lifted.contains(&unrelated));
    }
}
