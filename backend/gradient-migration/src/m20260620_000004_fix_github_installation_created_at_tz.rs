/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The create migration once typed `created_at` as `TIMESTAMPTZ`, while the entity is decoding
//! `TIMESTAMP`. The conversion is a no-op on installs created after that fix.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                DO $$
                BEGIN
                    IF (SELECT data_type FROM information_schema.columns
                        WHERE table_name = 'github_installation'
                          AND column_name = 'created_at') = 'timestamp with time zone'
                    THEN
                        ALTER TABLE github_installation
                            ALTER COLUMN created_at TYPE TIMESTAMP
                            USING created_at AT TIME ZONE 'UTC';
                    END IF;
                END $$;
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
                DO $$
                BEGIN
                    IF (SELECT data_type FROM information_schema.columns
                        WHERE table_name = 'github_installation'
                          AND column_name = 'created_at') = 'timestamp without time zone'
                    THEN
                        ALTER TABLE github_installation
                            ALTER COLUMN created_at TYPE TIMESTAMPTZ
                            USING created_at AT TIME ZONE 'UTC';
                    END IF;
                END $$;
                "#,
            )
            .await?;

        Ok(())
    }
}
