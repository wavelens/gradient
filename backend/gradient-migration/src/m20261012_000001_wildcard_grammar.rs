/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const NEW_GRAMMAR: &str = "replace(replace(wildcard, '*', '**'), '#', '*')";
const HAS_PATTERN: &str = "(wildcard LIKE '%*%' OR wildcard LIKE '%#%')";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(&format!(
            "UPDATE task SET wildcard = {NEW_GRAMMAR} WHERE {HAS_PATTERN} AND NOT managed"
        ))
        .await?;

        db.execute_unprepared(&format!(
            "UPDATE evaluation SET wildcard = {NEW_GRAMMAR} WHERE {HAS_PATTERN}"
        ))
        .await?;

        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
