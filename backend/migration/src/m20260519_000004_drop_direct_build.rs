/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Drop the legacy `direct_build` table. The build-request rework (issue
//! #234) replaced the direct-build flow with the build-request upload
//! endpoints; every evaluation now belongs to a project, so the table
//! has no readers or writers and can be removed.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(DirectBuild::Table).to_owned())
            .await
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "drop_direct_build is irreversible: the table has no readers in the new build-request flow".into(),
        ))
    }
}

#[derive(DeriveIden)]
enum DirectBuild {
    #[sea_orm(iden = "direct_build")]
    Table,
}
