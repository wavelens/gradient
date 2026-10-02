/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The rewrite is in place without a merge. `scope` is not part of `idx-metric_rollup-unique`, and
//! `scope_hash` is unchanged.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &str = "UPDATE metric_rollup SET scope = jsonb_build_object('project', scope->>'org') \
                  WHERE scope ? 'org'";

const DOWN: &str = "UPDATE metric_rollup SET scope = jsonb_build_object('org', scope->>'project') \
                    WHERE scope ? 'project'";

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
