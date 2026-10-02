/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: [&str; 4] = [
    "ALTER TABLE worker_registration ADD COLUMN token_encrypted text, ADD COLUMN gradient_ci boolean NOT NULL DEFAULT false",
    "ALTER TABLE base_worker ADD COLUMN token_encrypted text, ADD COLUMN gradient_ci boolean NOT NULL DEFAULT false",
    "CREATE UNIQUE INDEX idx_worker_registration_one_gradient_ci ON worker_registration (peer_id) WHERE gradient_ci",
    "CREATE UNIQUE INDEX idx_base_worker_one_gradient_ci ON base_worker (gradient_ci) WHERE gradient_ci",
];
const DOWN: [&str; 2] = [
    "ALTER TABLE worker_registration DROP COLUMN token_encrypted, DROP COLUMN gradient_ci",
    "ALTER TABLE base_worker DROP COLUMN token_encrypted, DROP COLUMN gradient_ci",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in UP {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for statement in DOWN {
            manager
                .get_connection()
                .execute_unprepared(statement)
                .await?;
        }

        Ok(())
    }
}
