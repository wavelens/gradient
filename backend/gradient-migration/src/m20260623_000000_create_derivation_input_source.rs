/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            CREATE TABLE derivation_input_source (
                id UUID PRIMARY KEY,
                derivation UUID NOT NULL REFERENCES derivation (id) ON DELETE CASCADE,
                hash TEXT NOT NULL,
                store_path TEXT NOT NULL,
                created_at TIMESTAMP NOT NULL,
                UNIQUE (derivation, hash)
            )
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"CREATE INDEX idx_derivation_input_source_hash ON derivation_input_source (hash)"#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP TABLE IF EXISTS derivation_input_source"#)
            .await?;

        Ok(())
    }
}
