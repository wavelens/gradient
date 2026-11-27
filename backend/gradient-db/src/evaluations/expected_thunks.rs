/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation_metric;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter, QueryOrder,
    QuerySelect, RelationTrait,
};

use gradient_types::*;

const SAMPLED_EVALUATIONS: u64 = 10;

pub async fn expected_thunks<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<Option<u64>, DbErr> {
    let recent = evaluation_metric::Entity::find()
        .join(
            JoinType::InnerJoin,
            evaluation_metric::Relation::Evaluation.def(),
        )
        .filter(CEvaluation::Task.eq(task))
        .filter(evaluation_metric::Column::TotalThunks.gt(0))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(SAMPLED_EVALUATIONS)
        .all(db)
        .await?;
    Ok(median(
        recent
            .into_iter()
            .filter_map(|m| u64::try_from(m.total_thunks).ok())
            .collect(),
    ))
}

pub(super) fn median(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    let mid = values.len() / 2;
    match values.len() {
        0 => None,
        n if n % 2 == 1 => Some(values[mid]),
        _ => Some(u64::midpoint(values[mid - 1], values[mid])),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_fixtures::shape::shape;

    #[tokio::test]
    async fn test_expected_thunks_is_median_of_newest_ten_nonzero_metrics_of_task_only() {
        let evaluations: Vec<EvaluationId> = (0..13).map(|_| EvaluationId::now_v7()).collect();
        let completed = EvaluationStatus::Completed;
        let mut seed = shape()
            .evaluation(evaluations[0])
            .evaluation_status(completed);
        for &evaluation in &evaluations[1..12] {
            seed = seed
                .evaluation(evaluation)
                .same_task_as(evaluations[0])
                .evaluation_status(completed);
        }
        let db = seed
            .evaluation(evaluations[12])
            .evaluation_status(completed)
            .into_database()
            .await;
        let thunks: [i64; 13] = [
            1_000_000, 100, 200, 300, 400, 500, 600, 700, 800, 900, 1000, 0, 5_000_000,
        ];
        for (age, (evaluation, total)) in evaluations.iter().zip(thunks).enumerate() {
            db.execute(&format!(
                "UPDATE evaluation SET created_at = created_at + interval '{age} minutes' \
                 WHERE id = '{evaluation}'"
            ))
            .await;
            crate::rows::insert::<evaluation_metric::Entity, _>(
                &db.connection(),
                MEvaluationMetric {
                    id: gradient_entity::ids::EvaluationMetricId::now_v7(),
                    evaluation: *evaluation,
                    total_thunks: total,
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        }
        let task = db.task_of(evaluations[0]).await;
        let other_task = db.task_of(evaluations[12]).await;

        let expected = expected_thunks(&db.connection(), task).await.unwrap();
        let other = expected_thunks(&db.connection(), other_task).await.unwrap();
        let unknown = expected_thunks(&db.connection(), TaskId::now_v7())
            .await
            .unwrap();
        db.finish().await;

        assert_eq!(expected, Some(550));
        assert_eq!(other, Some(5_000_000));
        assert_eq!(unknown, None);
    }

    #[test]
    fn an_even_sample_averages_the_two_middle_values() {
        assert_eq!(median(vec![40, 10, 30, 20]), Some(25));
    }
}
