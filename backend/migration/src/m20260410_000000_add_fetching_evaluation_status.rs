/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds the `Fetching` evaluation status (numeric value 8).
//!
//! This is a purely additive change — no existing rows use value 8, so no
//! data migration is required.  The migration exists only to mark the schema
//! version boundary.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260410_000000_add_fetching_evaluation_status"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        // No-op: value 8 (Fetching) is new and has no existing rows to migrate.
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Best-effort: rows stuck in Fetching (shouldn't exist at migration time)
        // are reset to Queued so they can be retried.
        manager
            .get_connection()
            .execute_unprepared("UPDATE evaluation SET status = 0 WHERE status = 8")
            .await?;
        Ok(())
    }
}
