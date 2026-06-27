/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `edges_unresolved` flag. `flush_deferred_deps` silently drops a dependency
//! edge whose dependency derivation isn't recorded (an interrupted/overlapping
//! eval never persisted it), yet `mark_edges_complete_for_eval` marked the
//! source anchor `edges_complete` anyway via its "is a build_job" branch - so an
//! anchor that declared dependencies but recorded zero edges was dispatched as
//! dependency-free and failed `InputsUnavailable` on an input the server never
//! had. The flag records "this anchor's edge set is known-incomplete" so both
//! `mark_edges_complete_for_eval` callers refuse to promote it until a complete
//! eval re-walks it and resolves the edges.

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
                "ALTER TABLE derivation_build
                   ADD COLUMN IF NOT EXISTS edges_unresolved boolean NOT NULL DEFAULT false",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE derivation_build DROP COLUMN IF EXISTS edges_unresolved")
            .await?;
        Ok(())
    }
}
