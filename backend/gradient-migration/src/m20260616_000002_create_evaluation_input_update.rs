/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Sidecar holding the worker-produced candidate lock for an `input_update` eval.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                CREATE TABLE evaluation_input_update (
                    id UUID PRIMARY KEY,
                    evaluation UUID NOT NULL UNIQUE REFERENCES evaluation (id) ON DELETE CASCADE,
                    base_commit TEXT NOT NULL,
                    generator TEXT NOT NULL,
                    target_inputs JSONB NOT NULL,
                    candidate_lock TEXT NULL,
                    bumped_inputs JSONB NULL,
                    created_at TIMESTAMP NOT NULL,
                    updated_at TIMESTAMP NOT NULL
                )
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP TABLE IF EXISTS evaluation_input_update"#)
            .await?;

        Ok(())
    }
}
