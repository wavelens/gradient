/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect, Value};
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

/// The set is [`BuildStatus::REQUEUEABLE`], which is including `Aborted`.
/// An evaluation with every shared build aborted would otherwise finalize `Completed`.
/// Nothing would have been built for that green check.
static EVAL_ANY_SHARED_BUILD_FAILED_SQL: LazyLock<String> = LazyLock::new(|| {
    format!(
        "SELECT EXISTS (SELECT 1 FROM build_job bj \
         JOIN derivation_build db ON db.id = bj.derivation_build \
         WHERE bj.evaluation = $1 AND db.status IN ({failed}) \
         AND NOT (db.status = {aborted} AND EXISTS (SELECT 1 FROM cluster_member cm \
              JOIN cluster_job cj ON cj.id = cm.cluster_job \
              WHERE cm.derivation_build = db.id AND cj.status = {completed}))) AS failed",
        failed = crate::sql::status::build_in(&BuildStatus::REQUEUEABLE),
        aborted = crate::sql::status::build(BuildStatus::Aborted),
        completed = i16::from(gradient_entity::cluster_job::ClusterJobStatus::Completed),
    )
});

crate::sql_lazy! {
    EVAL_ANY_SHARED_BUILD_FAILED = || EVAL_ANY_SHARED_BUILD_FAILED_SQL.as_str(),
        params = [EvaluationId];
}

pub async fn eval_blocked<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    flag(db, &EVAL_BLOCKED, evaluation, "blocked").await
}

pub async fn eval_any_shared_build_failed<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    flag(db, &EVAL_ANY_SHARED_BUILD_FAILED, evaluation, "failed").await
}

/// A missing row is an error, not `false`.
/// A silent "nothing blocks" would settle an evaluation with builds still running.
async fn flag<C: ConnectionTrait>(
    db: &C,
    query: &crate::sql::Query,
    evaluation: EvaluationId,
    column: &str,
) -> Result<bool, DbErr> {
    db.query_one_raw(query.bind([Value::Uuid(Some(evaluation.into_inner()))]))
        .await?
        .ok_or_else(|| DbErr::Custom(format!("{} returned no row", query.name)))?
        .try_get::<bool>("", column)
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
    fn an_aborted_shared_build_fails_the_evaluation_it_was_never_built_for() {
        let sql = EVAL_ANY_SHARED_BUILD_FAILED_SQL.as_str();
        let expected = crate::sql::status::build_in(&BuildStatus::REQUEUEABLE);
        assert!(
            sql.contains(&format!("db.status IN ({expected})")),
            "the verdict set is what a fresh evaluation would thaw: {sql}"
        );
        assert!(
            BuildStatus::REQUEUEABLE.contains(&BuildStatus::Aborted),
            "a shared build nothing built is not a success"
        );
        for ok in BuildStatus::TERMINAL_SUCCESS {
            assert!(
                !BuildStatus::REQUEUEABLE.contains(&ok),
                "{ok:?} must not fail its evaluation"
            );
        }
    }

    #[test]
    fn a_companion_of_a_completed_cluster_does_not_fail_its_evaluation() {
        let sql = EVAL_ANY_SHARED_BUILD_FAILED_SQL.as_str();

        assert!(sql.contains("FROM cluster_member cm"), "{sql}");
        assert!(sql.contains("AND NOT (db.status ="), "{sql}");
    }
}
