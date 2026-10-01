/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_types::*;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QuerySelect, Value};
use std::sync::LazyLock;

/// Whether an evaluation still names a shared build it is waiting for.
///
/// The exact answer behind the evaluation counters: asked only once they say
/// nothing blocks, so normally once per evaluation, and then it reads every
/// shared build the evaluation names. That is the working set it was handed, which is
/// what `Bulk` is for.
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

/// Whether any shared build the evaluation names ended without being built: what
/// decides `Failed` over `Completed` once [`eval_blocked`] says nothing is left.
///
/// The set is [`BuildStatus::REQUEUEABLE`], because the two questions are one
/// question seen from either end - what a fresh evaluation thaws is exactly what
/// this evaluation did not get built. `Aborted` is the member that matters and
/// the one this used to omit: `derivation_build` is global, so a shared build a
/// previous evaluation hard-aborted is already terminal when the next evaluation
/// names it, and an abort blocks nothing. An evaluation whose every shared build sat
/// `Aborted` therefore finalized `Completed` milliseconds after reaching
/// `Building` - a green check for a commit on which nothing was built. It is
/// deliberately NOT [`BuildStatus::TERMINAL_FAILURE`], which excludes `Aborted`
/// so an abort never cascades `DependencyFailed` downward. The one `Aborted`
/// shared build that is no failure: a companion stopped because its cluster job
/// completed through the primary member.
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

/// See [`EVAL_BLOCKED_SQL`]. An evaluation naming no shared build at all is not blocked.
pub async fn eval_blocked<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    flag(db, &EVAL_BLOCKED, evaluation, "blocked").await
}

/// See [`EVAL_ANY_SHARED_BUILD_FAILED_SQL`].
pub async fn eval_any_shared_build_failed<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<bool, DbErr> {
    flag(db, &EVAL_ANY_SHARED_BUILD_FAILED, evaluation, "failed").await
}

/// A one-row, one-column `EXISTS` read. A missing row is an error and not `false`:
/// an eval-done decision that silently read "nothing blocks" would settle an
/// evaluation whose builds are still running.
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

/// The shared build's current status, for the dispatcher's last look before a
/// hand-out: a queued job whose gate regressed since it was enqueued reads
/// `Created` here and is dropped instead of dispatched with a missing input.
pub async fn shared_build_status<C: ConnectionTrait>(
    db: &C,
    shared_build: DerivationBuildId,
) -> Result<Option<BuildStatus>, DbErr> {
    Ok(EDerivationBuild::find_by_id(shared_build)
        .one(db)
        .await?
        .map(|a| a.status))
}

/// Evaluations that reference `derivation` (via a `build_job`). Drives status
/// fan-out: a single shared build transition updates every referencing eval's view.
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

/// Bulk variant of [`evals_referencing_derivation`]: one chunked `IN` per batch
/// instead of a round-trip per derivation. The finalize fan-out asks for a whole
/// batch of derivations at once and only ever wants the union.
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

/// All `build_job` rows for `derivation`, across every evaluation that needs it.
pub async fn build_jobs_for_derivation<C: ConnectionTrait>(
    db: &C,
    derivation: DerivationId,
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Derivation.eq(derivation))
        .all(db)
        .await
}

/// Bulk variant of [`build_jobs_for_derivation`]: one IN-list query for the
/// whole batch instead of a round-trip per derivation.
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

/// Whether any surviving evaluation names `derivation` (a `build_job` exists), so
/// promotion can schedule it.
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

    /// `derivation_build` is global, so a shared build a previous evaluation aborted
    /// is terminal before the next evaluation ever dispatches it - and an abort
    /// blocks nothing. Omitting `Aborted` here finalized such an evaluation
    /// `Completed`: 26 of 26 shared builds aborted, nothing built, a green check.
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
