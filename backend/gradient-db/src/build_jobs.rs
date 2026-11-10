/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::sea_query::Query;
use sea_orm::{ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter};
use std::collections::HashMap;

pub async fn of_evaluations<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Evaluation.is_in(evaluations.iter().copied()))
        .all(db)
        .await
}

pub async fn of_derivations_in_evaluations<C: ConnectionTrait>(
    db: &C,
    evaluations: &[EvaluationId],
    derivations: &[DerivationId],
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Evaluation.is_in(evaluations.iter().copied()))
        .filter(CBuildJob::Derivation.is_in(derivations.iter().copied()))
        .all(db)
        .await
}

pub async fn of_derivations_in_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    derivations: &[DerivationId],
) -> Result<Vec<MBuildJob>, DbErr> {
    crate::fetch_in_chunks(derivations, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::Evaluation.eq(evaluation))
            .filter(CBuildJob::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
}

pub async fn of_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .all(db)
        .await
}

pub async fn by_id_in_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    id: BuildJobId,
) -> Result<Option<MBuildJob>, DbErr> {
    EBuildJob::find_by_id(id)
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .one(db)
        .await
}

pub async fn newest_other_non_aborted_evaluation_by_shared_build_id<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    evaluation: EvaluationId,
    derivation_builds: &[DerivationBuildId],
) -> Result<HashMap<DerivationBuildId, EvaluationId>, DbErr> {
    let jobs = crate::fetch_in_chunks(derivation_builds, |chunk| async move {
        EBuildJob::find()
            .filter(CBuildJob::DerivationBuild.is_in(chunk))
            .filter(CBuildJob::Evaluation.ne(evaluation))
            .filter(
                CBuildJob::Evaluation.in_subquery(
                    Query::select()
                        .column(CEvaluation::Id)
                        .from(gradient_entity::evaluation::Entity)
                        .and_where(CEvaluation::Task.eq(task))
                        .to_owned(),
                ),
            )
            .filter(CBuildJob::Aborted.eq(false))
            .all(db)
            .await
    })
    .await?;
    let mut newest: HashMap<DerivationBuildId, MBuildJob> = HashMap::new();
    for job in jobs {
        match newest.get(&job.derivation_build) {
            Some(kept) if kept.created_at >= job.created_at => {}
            _ => {
                newest.insert(job.derivation_build, job);
            }
        }
    }
    Ok(newest
        .into_iter()
        .map(|(build, job)| (build, job.evaluation))
        .collect())
}

pub async fn of_derivation_builds_in_evaluation<C: ConnectionTrait>(
    db: &C,
    evaluation: EvaluationId,
    derivation_builds: Vec<DerivationBuildId>,
) -> Result<Vec<MBuildJob>, DbErr> {
    EBuildJob::find()
        .filter(CBuildJob::Evaluation.eq(evaluation))
        .filter(CBuildJob::DerivationBuild.is_in(derivation_builds))
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::shape::shape;

    const A: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";

    #[tokio::test]
    async fn the_shared_build_continues_in_a_later_evaluation_of_the_same_task() {
        let (aborted, later, elsewhere) = (
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
            EvaluationId::now_v7(),
        );
        let db = shape()
            .evaluation(aborted)
            .evaluation_status(gradient_entity::evaluation::EvaluationStatus::Aborted)
            .derivation(A)
            .job()
            .evaluation(later)
            .same_task_as(aborted)
            .derivation(A)
            .job()
            .evaluation(elsewhere)
            .derivation(A)
            .job()
            .into_database()
            .await;
        let task = db.task_of(aborted).await;
        let shared = db.shared_build(A).await.id;

        let continued = newest_other_non_aborted_evaluation_by_shared_build_id(
            &db.connection(),
            task,
            aborted,
            &[shared],
        )
        .await
        .expect("the query runs");

        assert_eq!(continued.get(&shared), Some(&later));
        db.finish().await;
    }
}
