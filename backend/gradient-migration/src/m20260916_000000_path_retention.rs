/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `cache_derivation.last_fetched_at` has no reader: fetch recency lives on
//! `cached_path_signature` and the eviction pass reads it there.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "ALTER TABLE cache_derivation DROP COLUMN IF EXISTS last_fetched_at";
const DOWN: &str =
    "ALTER TABLE cache_derivation ADD COLUMN IF NOT EXISTS last_fetched_at timestamp";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{DOWN, UP};

    #[test]
    fn down_restores_exactly_what_up_dropped() {
        assert!(UP.contains("DROP COLUMN IF EXISTS last_fetched_at"), "{UP}");
        assert!(
            DOWN.contains("ADD COLUMN IF NOT EXISTS last_fetched_at timestamp"),
            "{DOWN}"
        );
    }
}
