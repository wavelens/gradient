/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tests for organization entity

use chrono::NaiveDate;
use entity::*;
use sea_orm::{DatabaseBackend, MockDatabase, entity::prelude::*};
use uuid::Uuid;

#[tokio::test]
async fn test_organization_entity_basic() -> Result<(), DbErr> {
    let org_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let naive_date = NaiveDate::from_ymd_opt(2024, 1, 1)
        .unwrap()
        .and_hms_opt(0, 0, 0)
        .unwrap();

    let db = MockDatabase::new(DatabaseBackend::Postgres)
        .append_query_results([vec![organization::Model {
            id: org_id,
            name: "test-org".to_owned(),
            display_name: "Test Organization".to_owned(),
            description: "Test Description".to_owned(),
            public_key: "ssh-rsa AAAAB3...".to_owned(),
            private_key: "-----BEGIN PRIVATE KEY-----".to_owned(),
            use_nix_store: true,
            created_by: user_id,
            created_at: naive_date,
        }]])
        .into_connection();

    let result = organization::Entity::find_by_id(org_id).one(&db).await?;

    assert!(result.is_some());
    let org = result.unwrap();
    assert_eq!(org.name, "test-org");
    assert_eq!(org.display_name, "Test Organization");
    assert_eq!(org.created_by, user_id);

    Ok(())
}
