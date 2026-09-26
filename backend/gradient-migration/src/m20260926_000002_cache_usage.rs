/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Stored bytes per cache, recounted by the rollup pass so a dashboard read sums
//! one row per visible cache instead of every cached path behind it.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &["CREATE TABLE IF NOT EXISTS cache_usage ( \
     cache uuid PRIMARY KEY REFERENCES cache(id) ON DELETE CASCADE, \
     bytes bigint NOT NULL)"];

const DOWN: &[&str] = &["DROP TABLE IF EXISTS cache_usage"];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in UP {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in DOWN {
            manager.get_connection().execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
