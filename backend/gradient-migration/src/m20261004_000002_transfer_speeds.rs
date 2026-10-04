/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: [&str; 2] = [
    "ALTER TABLE derivation_metric DROP COLUMN peak_network_mbps",
    "ALTER TABLE worker_sample DROP COLUMN network_speed_mbps, \
     ADD COLUMN upload_speed_mbps real, ADD COLUMN download_speed_mbps real",
];

const DOWN: [&str; 2] = [
    "ALTER TABLE worker_sample DROP COLUMN upload_speed_mbps, \
     DROP COLUMN download_speed_mbps, ADD COLUMN network_speed_mbps real",
    "ALTER TABLE derivation_metric ADD COLUMN peak_network_mbps double precision",
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
