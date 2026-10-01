/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The ledger of storage migrations, and the checkpoint a deep GC round resumes
//! from after a restart.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"CREATE TABLE IF NOT EXISTS storage_migration (
                   name text PRIMARY KEY,
                   checkpoint text,
                   started_at timestamp NOT NULL,
                   applied_at timestamp
               )"#,
        )
        .await?;
        db.execute_unprepared("ALTER TABLE admin_task ADD COLUMN IF NOT EXISTS checkpoint text")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared("ALTER TABLE admin_task DROP COLUMN IF EXISTS checkpoint")
            .await?;
        db.execute_unprepared("DROP TABLE IF EXISTS storage_migration")
            .await?;

        Ok(())
    }
}
