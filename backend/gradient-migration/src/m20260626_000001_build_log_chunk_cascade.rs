/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `build_log_chunk` carried a bare `build_attempt` UUID with no FK, so its rows
//! leaked forever once an evaluation (and its attempts) were GC'ed. Purge the
//! existing orphans, then add the missing cascade to complete the
//! `evaluation -> build_job -> build_attempt -> build_log_chunk` chain.

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
                DELETE FROM build_log_chunk c
                  WHERE NOT EXISTS (SELECT 1 FROM build_attempt a WHERE a.id = c.build_attempt);
                ALTER TABLE build_log_chunk
                  ADD CONSTRAINT "fk-build_log_chunk-build_attempt"
                  FOREIGN KEY (build_attempt) REFERENCES build_attempt (id) ON DELETE CASCADE;
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"ALTER TABLE build_log_chunk DROP CONSTRAINT "fk-build_log_chunk-build_attempt""#,
            )
            .await?;

        Ok(())
    }
}
