/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Disable JIT for gradient's database. The dispatch-tick reconcile fixpoints
//! and the cached_path consistency sweep run correlated `NOT EXISTS` predicates
//! over the whole build graph, so their cost estimates trip Postgres's JIT
//! thresholds - yet they execute sub-second, making per-call JIT compilation
//! pure overhead (measured 2.9s -> 0.6s on the closure_complete CLEAR). JIT
//! never pays for this OLTP-shaped workload. Scope via `current_database()` so
//! it is name-portable across prod/CI/dev; owner role `gradient` may ALTER it.

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
