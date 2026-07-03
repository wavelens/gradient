/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Squashed baseline replacing the 151 pre-globalization migrations
//! (m20241107_135027 through m20260619_000001). On a fresh database it emits
//! the schema exactly as that chain left it (verified by pg_dump diff), so the
//! post-globalization migrations replay on top unchanged. On an
//! already-provisioned database it is a no-op: the schema exists, and
//! `prune_removed_migrations` (gradient-db connection.rs) has already dropped
//! the deleted files' `seaql_migrations` rows. Databases that stopped mid-way
//! through the pre-globalization chain must first upgrade through a release
//! that still ships it (see docs/src/migrations.md).

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let provisioned = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                "SELECT to_regclass('public.organization') IS NOT NULL AS provisioned",
            ))
            .await?
            .and_then(|r| r.try_get::<bool>("", "provisioned").ok())
            .unwrap_or(false);
        if provisioned {
            return Ok(());
        }

        db.execute_unprepared(include_str!("m20241101_000000_baseline.sql"))
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "m20241101_000000_baseline is irreversible".into(),
        ))
    }
}
