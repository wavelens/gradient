/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::{DbContext, graph::closure::transitive_closure_reachable};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult};

pub const PRIORITIZABLE: [BuildStatus; 4] = [
    BuildStatus::Created,
    BuildStatus::Queued,
    BuildStatus::Building,
    BuildStatus::FailedTransient,
];

#[derive(FromQueryResult)]
struct SharedBuildRow {
    id: uuid::Uuid,
}

fn prioritize_evaluation_sql() -> String {
    format!(
        "UPDATE evaluation SET prioritized = true \
         WHERE id = $1 AND status NOT IN ({}) RETURNING id",
        crate::sql::status::eval_in(&[
            EvaluationStatus::Completed,
            EvaluationStatus::Failed,
            EvaluationStatus::Aborted,
        ])
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

    let rows = SharedBuildRow::find_by_statement(EVALUATION_OPEN_SHARED_BUILDS.bind([id]))
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

    let rows = SharedBuildRow::find_by_statement(PRIORITIZE_SHARED_BUILDS.bind([closure.into()]))
        .all(&ctx.worker_db)
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
}
