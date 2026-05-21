/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Widen `evaluation.waiting_reason` to `jsonb` and tag every legacy row with
//! `kind = "workers"`.
//!
//! `WaitingReason` was a flat struct (`unmet`, `connected_workers`,
//! `available_architectures`) and is now a `#[serde(tag = "kind")]` enum with
//! `Workers`, `Approval`, `NoCache` variants. The column was originally added
//! as `json` (m20260506); the `?` containment operator and `jsonb_set` we use
//! to backfill the tag require `jsonb`, and we want the queryable form going
//! forward anyway. So this migration first converts the column type, then
//! backfills the `kind` tag on rows that predate the enum.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();

        db.execute(Statement::from_string(
            backend,
            "ALTER TABLE evaluation \
             ALTER COLUMN waiting_reason TYPE jsonb \
             USING waiting_reason::jsonb",
        ))
        .await?;

        db.execute(Statement::from_string(
            backend,
            "UPDATE evaluation \
             SET waiting_reason = jsonb_set(waiting_reason, '{kind}', '\"workers\"'::jsonb) \
             WHERE waiting_reason IS NOT NULL \
               AND waiting_reason ? 'unmet' \
               AND NOT (waiting_reason ? 'kind')",
        ))
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();

        db.execute(Statement::from_string(
            backend,
            "UPDATE evaluation \
             SET waiting_reason = waiting_reason - 'kind' \
             WHERE waiting_reason IS NOT NULL \
               AND waiting_reason->>'kind' = 'workers'",
        ))
        .await?;

        db.execute(Statement::from_string(
            backend,
            "ALTER TABLE evaluation \
             ALTER COLUMN waiting_reason TYPE json \
             USING waiting_reason::json",
        ))
        .await?;
        Ok(())
    }
}
