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

fn median(mut values: Vec<u64>) -> Option<u64> {
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
    use sea_orm::{DatabaseBackend, MockDatabase};

    fn metric(total_thunks: i64) -> MEvaluationMetric {
        MEvaluationMetric {
            total_thunks,
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn the_median_of_the_recent_evaluations_gives_the_expected_thunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![
                metric(30_000_000),
                metric(450_000_000),
                metric(160_000_000),
            ]])
            .into_connection();

        let expected = expected_thunks(&db, TaskId::now_v7()).await.unwrap();

        assert_eq!(expected, Some(160_000_000));
        let log = db.into_transaction_log();
        assert_eq!(log.len(), 1);
        let sql = format!("{:?}", log[0]);
        assert!(sql.contains("INNER JOIN"), "{sql}");
        assert!(sql.contains("ORDER BY"), "{sql}");
        assert!(!sql.contains("\"status\""), "{sql}");
    }

    #[tokio::test]
    async fn a_task_without_evaluation_metrics_has_no_expected_thunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MEvaluationMetric>::new()])
            .into_connection();

        assert_eq!(expected_thunks(&db, TaskId::now_v7()).await.unwrap(), None);
    }

    #[test]
    fn an_even_sample_averages_the_two_middle_values() {
        assert_eq!(median(vec![40, 10, 30, 20]), Some(25));
    }
}
