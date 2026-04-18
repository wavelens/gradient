/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260418_000000_rename_feature_add_kind"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Rename feature → system_requirement.
        // PostgreSQL foreign keys track by OID, so dependent tables
        // (derivation_feature, build_feature) keep working without changes.
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE feature RENAME TO system_requirement;

                ALTER TABLE system_requirement
                    ADD COLUMN kind VARCHAR NOT NULL DEFAULT 'feature';

                ALTER TABLE system_requirement
                    ADD CONSTRAINT system_requirement_name_kind_unique
                    UNIQUE (name, kind);
                "#,
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE system_requirement
                    DROP CONSTRAINT IF EXISTS system_requirement_name_kind_unique;

                ALTER TABLE system_requirement
                    DROP COLUMN kind;

                ALTER TABLE system_requirement RENAME TO feature;
                "#,
            )
            .await?;
        Ok(())
    }
}
