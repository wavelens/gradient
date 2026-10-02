/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! This baseline is replacing the 151 pre-globalization migrations and is a no-op on an
//! already-provisioned database. A database stopped mid-chain must first upgrade through a release
//! still shipping that chain.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let provisioned = db
            .query_one_raw(Statement::from_string(
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
