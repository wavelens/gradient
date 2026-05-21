/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Tag every legacy `evaluation.waiting_reason` row with `kind = "workers"`.
//!
//! `WaitingReason` was a flat struct (`unmet`, `connected_workers`,
//! `available_architectures`) and is now a `#[serde(tag = "kind")]` enum with
//! `Workers`, `Approval`, `NoCache` variants. Existing rows lack the `kind`
//! discriminator; they all represent the historical workers-capacity reason,
//! so we add it in-place. The application's `WaitingReason::from_json` is
//! tolerant of untagged legacy rows, but the migration backfills the tag so
//! every row matches the canonical shape going forward.

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
        Ok(())
    }
}
