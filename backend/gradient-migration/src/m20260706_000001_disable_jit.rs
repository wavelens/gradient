/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! JIT is disabled because the correlated `NOT EXISTS` sweeps are tripping its cost thresholds
//! while running sub-second. Disabling it cut the closure_complete CLEAR from 2.9s to 0.6s.
//! `current_database()` is keeping the statement portable across prod, CI and dev.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I SET jit = off', current_database()); END $$;",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "DO $$ BEGIN EXECUTE format('ALTER DATABASE %I RESET jit', current_database()); END $$;",
            )
            .await?;
        Ok(())
    }
}
