/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Unique indexes back every key tested by a read before a write.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: [&str; 6] = [
    "CREATE UNIQUE INDEX idx_api_owned_by_name_unique ON api (owned_by, name)",
    r#"CREATE UNIQUE INDEX idx_project_user_project_user_unique ON project_user (project, "user")"#,
    "CREATE UNIQUE INDEX idx_project_cache_project_cache_unique ON project_cache (project, cache)",
    "CREATE UNIQUE INDEX idx_role_project_name_unique ON role (project, name) WHERE project IS NOT NULL",
    "CREATE UNIQUE INDEX idx_cache_role_cache_name_unique ON cache_role (cache, name) WHERE cache IS NOT NULL",
    "CREATE UNIQUE INDEX idx_worker_registration_peer_id_worker_id_unique ON worker_registration (peer_id, worker_id)",
];

const DOWN: [&str; 6] = [
    "DROP INDEX idx_worker_registration_peer_id_worker_id_unique",
    "DROP INDEX idx_cache_role_cache_name_unique",
    "DROP INDEX idx_role_project_name_unique",
    "DROP INDEX idx_project_cache_project_cache_unique",
    "DROP INDEX idx_project_user_project_user_unique",
    "DROP INDEX idx_api_owned_by_name_unique",
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
