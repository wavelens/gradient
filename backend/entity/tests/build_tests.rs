/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for build entity

use chrono::NaiveDate;
use entity::*;
use sea_orm::{DatabaseBackend, MockDatabase, entity::prelude::*};
use uuid::Uuid;

#[tokio::test]
async fn test_build_entity_with_status() -> Result<(), DbErr> {
    let build_id = Uuid::new_v4();
    let evaluation_id = Uuid::new_v4();
    let server_id = Uuid::new_v4();
    let naive_date = NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![build::Model {
            id: build_id,
            evaluation: evaluation_id,
            status: build::BuildStatus::Completed,
            derivation_path: "/nix/store/abc123-hello-world".to_owned(),
            architecture: server::Architecture::X86_64Linux,
            server: Some(server_id),
            log: Some("Build completed successfully".to_owned()),
            created_at: naive_date,
            updated_at: naive_date,
        }]])
        .into_connection();

    let result = build::Entity::find_by_id(build_id).one(&db).await?;

    assert!(result.is_some());
    let build = result.unwrap();
    assert_eq!(build.derivation_path, "/nix/store/abc123-hello-world");
    assert_eq!(build.status, build::BuildStatus::Completed);
    assert_eq!(build.evaluation, evaluation_id);
    assert_eq!(build.server, Some(server_id));

    Ok(())
}
