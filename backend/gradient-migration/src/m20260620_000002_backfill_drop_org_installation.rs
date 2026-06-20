/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            "INSERT INTO github_installation (id, organization, installation_id, account_login, created_by, created_at) \
             SELECT uuidv7(), o.id, o.github_installation_id, NULL, o.created_by, NOW() \
             FROM organization o \
             WHERE o.github_installation_id IS NOT NULL \
             ON CONFLICT (organization, installation_id) DO NOTHING",
        )
        .await?;

        db.execute_unprepared(
            "UPDATE integration i \
             SET github_installation = gi.id \
             FROM organization o \
             JOIN github_installation gi ON gi.organization = o.id \
                 AND gi.installation_id = o.github_installation_id \
             WHERE i.organization = o.id AND i.forge_type = 3 AND i.github_installation IS NULL",
        )
        .await?;

        db.execute_unprepared("ALTER TABLE organization DROP COLUMN github_installation_id")
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("ALTER TABLE organization ADD COLUMN github_installation_id BIGINT")
            .await?;
        Ok(())
    }
}
