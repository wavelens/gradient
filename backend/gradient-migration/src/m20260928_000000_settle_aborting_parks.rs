/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The `aborting` waiting reason is gone: an evaluation left parked under it by
//! an interrupted abort finishes as `Aborted` instead of being resumed.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"UPDATE evaluation
     SET status = CASE WHEN status = 4 THEN 7 ELSE status END,
         finished_at = CASE WHEN status = 4
             THEN coalesce(finished_at, (now() AT TIME ZONE 'UTC')) ELSE finished_at END,
         waiting_reason = NULL,
         updated_at = (now() AT TIME ZONE 'UTC')
     WHERE waiting_reason->>'kind' = 'aborting'"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
