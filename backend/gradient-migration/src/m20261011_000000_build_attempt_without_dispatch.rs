/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A build failed before any worker took it has an attempt without a dispatched job.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const ALLOW_NULL: &str = "ALTER TABLE build_attempt ALTER COLUMN dispatched_job DROP NOT NULL";

const DELETE_UNDISPATCHED: &str = "DELETE FROM build_attempt WHERE dispatched_job IS NULL";

const FORBID_NULL: &str = "ALTER TABLE build_attempt ALTER COLUMN dispatched_job SET NOT NULL";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(ALLOW_NULL)
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(DELETE_UNDISPATCHED).await?;
        db.execute_unprepared(FORBID_NULL).await?;
        Ok(())
    }
}
