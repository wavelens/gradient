/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for user entity

use chrono::NaiveDate;
use entity::*;
use sea_orm::{DatabaseBackend, MockDatabase, entity::prelude::*};
use uuid::Uuid;

#[tokio::test]
async fn test_user_entity_basic() -> Result<(), DbErr> {
    let user_id = Uuid::new_v4();
    let naive_date = NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![user::Model {
            id: user_id,
            username: "testuser".to_owned(),
            name: "Test User".to_owned(),
            email: "test@example.com".to_owned(),
            password: Some("hashed_password".to_owned()),
            last_login_at: naive_date,
            created_at: naive_date,
        }]])
        .into_connection();

    let result = user::Entity::find_by_id(user_id).one(&db).await?;

    assert!(result.is_some());
    let user = result.unwrap();
    assert_eq!(user.username, "testuser");
    assert_eq!(user.email, "test@example.com");

    Ok(())
}
