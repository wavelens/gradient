/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::evaluations::EvaluationItemFailureAndAbortFlags;
use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect};
use std::sync::LazyLock;

static EVAL_BLOCKED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM build_job bj \
         JOIN derivation_build db ON db.id = bj.derivation_build \
         WHERE bj.evaluation = $1 AND {blocks}) AS blocked",
        blocks = crate::graph::predicates::blocks_evaluation_predicate("db"),
    )
});

crate::sql_lazy! {
    EVAL_BLOCKED = || EVAL_BLOCKED_SQL.as_str(),
        params = [EvaluationId],
        tier = Bulk,
        budget = crate::sql::Budget::bulk().buffers(500_000)
            .because("an evaluation nothing blocks is read to its last shared build, and the \
                      fixture's largest names ~98k");
}

/// Shared builds with a user abort set the evaluation to Aborted, not Failed. Builds that depend on an aborted build get DependencyFailed status. DependencyFailed can count as a failure only when there are no aborts in that evaluation. The aborted companions of a completed cluster job count as neither.
static SELECT_SHARED_BUILD_FAILED_AND_ABORTED_FLAGS_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "WITH outcome AS (SELECT \
           COALESCE(bool_or(db.status IN ({failed})), false) AS failed, \
           COALESCE(bool_or(db.status = {dependency_failed}), false) AS dependency_failed, \
           COALESCE(bool_or(db.status = {aborted} AND NOT EXISTS (SELECT 1 FROM cluster_member cm \
              JOIN cluster_job cj ON cj.id = cm.cluster_job \
              WHERE cm.derivation_build = db.id AND cj.status = {completed})), false) AS aborted \
         FROM build_job bj \
         JOIN derivation_build db ON db.id = bj.derivation_build \
         WHERE bj.evaluation = $1 AND db.status IN ({unfinished})) \
         SELECT failed OR (dependency_failed AND NOT aborted) AS failed, aborted FROM outcome",
        failed = crate::sql::status::build_in(&[
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTimeout,
        ]),
        dependency_failed = crate::sql::status::build(BuildStatus::DependencyFailed),
        aborted = crate::sql::status::build(BuildStatus::Aborted),
        unfinished = crate::sql::status::build_in(&BuildStatus::REQUEUEABLE),
        completed = i16::from(gradient_entity::cluster_job::ClusterJobStatus::Completed),
    )
});

crate::sql_lazy! {
    SELECT_SHARED_BUILD_FAILED_AND_ABORTED_FLAGS =
        || SELECT_SHARED_BUILD_FAILED_AND_ABORTED_FLAGS_SQL.as_str(),
        params = [EvaluationId];
}

pub async fn eval_blocked<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    EVAL_BLOCKED
        .scalar(db, [evaluation.into()], "blocked")
        .await
}

pub async fn read_shared_build_failure_and_abort_flags<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<EvaluationItemFailureAndAbortFlags, DbErr> {
    EvaluationItemFailureAndAbortFlags::read(
        db,
        &SELECT_SHARED_BUILD_FAILED_AND_ABORTED_FLAGS,
        evaluation,
    )
    .await
}

pub async fn shared_build_status<C: ConnectionTrait>(
    db: &C,
    shared_build: DerivationBuildId,
) -> Result<Option<BuildStatus>, DbErr> {
    Ok(EDerivationBuild::find_by_id(shared_build)
        .one(db)
        .await?
        .map(|a| a.status))
}

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

pub async fn build_jobs_for_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Derivation.eq(derivation))
        .all(db)
        .await
}

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

pub async fn derivations_by_id<C: ConnectionTrait>(
    db: &C,
    ids: &[DerivationId],
) -> Result<std::collections::HashMap<DerivationId, MDerivation>, DbErr> {
    Ok(crate::fetch_in_chunks(ids, |chunk| async move {
        EDerivation::find()
            .filter(CDerivation::Id.is_in(chunk))
            .all(db)
            .await
    })
    .await?
    .into_iter()
    .map(|derivation| (derivation.id, derivation))
    .collect())
}

pub async fn shared_builds_by_derivation<C: ConnectionTrait>(
    db: &C,
    derivations: &[DerivationId],
) -> Result<std::collections::HashMap<DerivationId, MDerivationBuild>, DbErr> {
    Ok(crate::fetch_in_chunks(derivations, |chunk| async move {
        EDerivationBuild::find()
            .filter(CDerivationBuild::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await?
    .into_iter()
    .map(|shared_build| (shared_build.derivation, shared_build))
    .collect())
}

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aborted_shared_build_ends_evaluation_aborted_and_sql_reads_aborted_separately_from_failed() {
        let sql = SELECT_SHARED_BUILD_FAILED_AND_ABORTED_FLAGS_SQL.as_str();
        let failed = crate::sql::status::build_in(&[
            BuildStatus::FailedPermanent,
            BuildStatus::FailedTimeout,
        ]);
        let aborted = crate::sql::status::build(BuildStatus::Aborted);
        let unfinished = crate::sql::status::build_in(&BuildStatus::REQUEUEABLE);

        assert!(sql.contains(&format!("db.status IN ({failed})")), "{sql}");
        assert!(sql.contains(&format!("db.status = {aborted}")), "{sql}");
        assert!(
            sql.contains(&format!("db.status IN ({unfinished})")),
            "{sql}"
        );
        assert!(
            sql.contains("failed OR (dependency_failed AND NOT aborted) AS failed"),
            "{sql}"
        );
        for ok in BuildStatus::TERMINAL_SUCCESS {
            assert!(!BuildStatus::REQUEUEABLE.contains(&ok), "{ok:?}");
        }
    }

    #[test]
    fn aborted_cluster_companion_is_neither_aborted_nor_failed() {
        let sql = SELECT_SHARED_BUILD_FAILED_AND_ABORTED_FLAGS_SQL.as_str();

        assert!(sql.contains("FROM cluster_member cm"), "{sql}");
        assert!(
            sql.contains("AND NOT EXISTS (SELECT 1 FROM cluster_member cm"),
            "{sql}"
        );
    }
}
