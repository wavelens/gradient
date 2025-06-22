/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for evaluation entity

use chrono::NaiveDate;
use entity::*;
use sea_orm::{DatabaseBackend, MockDatabase, entity::prelude::*};
use uuid::Uuid;

#[tokio::test]
async fn test_evaluation_entity_with_status() -> Result<(), DbErr> {
    let eval_id = Uuid::new_v4();
    let project_id = Uuid::new_v4();
    let commit_id = Uuid::new_v4();
    let naive_date = NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![evaluation::Model {
            id: eval_id,
            project: project_id,
            repository: "https://github.com/test/repo".to_owned(),
            commit: commit_id,
            wildcard: "*".to_owned(),
            status: evaluation::EvaluationStatus::Completed,
            previous: None,
            next: None,
            created_at: naive_date,
        }]])
        .into_connection();

    let result = evaluation::Entity::find_by_id(eval_id).one(&db).await?;

    assert!(result.is_some());
    let eval = result.unwrap();
    assert_eq!(eval.status, evaluation::EvaluationStatus::Completed);
    assert_eq!(eval.project, project_id);
    assert_eq!(eval.commit, commit_id);
    assert_eq!(eval.repository, "https://github.com/test/repo");

    Ok(())
}
