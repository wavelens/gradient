/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

const PROVISIONED_MARKER: &str = "team";

fn provisioned_sql() -> String {
    format!("SELECT to_regclass('public.{PROVISIONED_MARKER}') IS NOT NULL AS provisioned")
}

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let provisioned = db
            .query_one_raw(Statement::from_string(
                db.get_database_backend(),
                provisioned_sql(),
            ))
            .await?
            .and_then(|r| r.try_get::<bool>("", "provisioned").ok())
            .unwrap_or(false);
        if provisioned {
            return Ok(());
        }

        db.execute_unprepared(include_str!("m20261013_000000_baseline.sql"))
            .await?;
        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "m20261013_000000_baseline is irreversible".into(),
        ))
    }
}
