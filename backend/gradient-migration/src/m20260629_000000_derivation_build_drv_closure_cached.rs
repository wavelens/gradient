/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `drv_closure_cached` flag. A build worker cannot even import a build target's
//! `.drv` until that `.drv`'s full reference closure - every transitive input
//! `.drv` plus its input sources - is present locally, because the nix daemon's
//! `add_to_store_nar` rejects a NAR whose declared references are absent. The
//! eval pushes those `.drv`s progressively, so dispatch could race ahead and a
//! build attempted before its `.drv` closure landed failed terminal
//! `InputsUnavailable`. The flag is the `.drv`-closure analogue of
//! `closure_complete` (which tracks the OUTPUT closure): true once this anchor's
//! own `.drv` is cached and every build-dependency is itself `drv_closure_cached`.
//! Dispatch gates non-substitutable anchors on it.

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
                   ADD COLUMN IF NOT EXISTS drv_closure_cached boolean NOT NULL DEFAULT false",
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "ALTER TABLE derivation_build DROP COLUMN IF EXISTS drv_closure_cached",
            )
            .await?;
        Ok(())
    }
}
