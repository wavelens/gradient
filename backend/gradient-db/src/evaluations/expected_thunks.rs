/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_metric;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbErr, EntityTrait, JoinType, QueryFilter, QueryOrder,
    QuerySelect, RelationTrait,
};

use gradient_types::*;

pub async fn expected_thunks<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
) -> Result<Option<u64>, DbErr> {
    let latest = evaluation_metric::Entity::find()
        .join(
            JoinType::InnerJoin,
            evaluation_metric::Relation::Evaluation.def(),
        )
        .filter(CEvaluation::Task.eq(task))
        .filter(CEvaluation::Status.eq(EvaluationStatus::Completed))
        .order_by_desc(CEvaluation::CreatedAt)
        .limit(1)
        .all(db)
        .await?;
    Ok(latest
        .into_iter()
        .next()
        .and_then(|m| u64::try_from(m.total_thunks).ok()))
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
    async fn the_newest_completed_evaluation_of_the_task_gives_the_expected_thunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![metric(450_000_000)]])
            .into_connection();

        let expected = expected_thunks(&db, TaskId::now_v7()).await.unwrap();

        assert_eq!(expected, Some(450_000_000));
        let log = db.into_transaction_log();
        assert_eq!(log.len(), 1);
        let sql = format!("{:?}", log[0]);
        assert!(sql.contains("INNER JOIN"), "{sql}");
        assert!(sql.contains("ORDER BY"), "{sql}");
    }

    #[tokio::test]
    async fn a_task_without_a_completed_evaluation_has_no_expected_thunks() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MEvaluationMetric>::new()])
            .into_connection();

        assert_eq!(expected_thunks(&db, TaskId::now_v7()).await.unwrap(), None);
    }
}
