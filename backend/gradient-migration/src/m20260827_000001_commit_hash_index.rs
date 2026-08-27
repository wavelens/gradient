/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Index `commit.hash` so `GET /tasks/{project}/{task}/evaluations?commit=` can
//! resolve a hash without a sequential scan. The table carried only its primary
//! key, and a fresh row is written per evaluation, so it grows with evaluation
//! history and one hash maps to many ids.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("CREATE INDEX \"idx-commit-hash\" ON public.commit (hash)")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DROP INDEX IF EXISTS \"idx-commit-hash\"")
            .await?;

        Ok(())
    }
}
