/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Re-key `metric_rollup.scope` from `{'org': id}` to `{'project': id}` (#571).
//!
//! The organization -> project rename moved tables and columns but not the
//! jsonb scope payload the rollup writes, and the rollup's `ON CONFLICT DO
//! UPDATE` clauses never refreshed `scope`, so rows split into two shapes over
//! one unchanged `scope_hash` (hashed over the bare id). The minute->hour->day
//! cascade groups by `scope` as well as `scope_hash`, so a bucket spanning the
//! rename produced two rows per conflict key and Postgres rejected the whole
//! statement with "ON CONFLICT DO UPDATE command cannot affect row a second
//! time"; `(scope->>'project')` read-side filters missed every pre-rename row
//! on top of that.
//!
//! `scope` is not part of `idx-metric_rollup-unique` and `scope_hash` is
//! unchanged, so this rewrites in place with no merge.

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
