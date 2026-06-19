/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build-once anchor: one `derivation_build` row per derivation (UNIQUE),
//! seeded in `Created` from the existing global derivations. The real status
//! is recomputed by the next evaluation.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            CREATE TABLE derivation_build (
                id UUID PRIMARY KEY,
                derivation UUID NOT NULL UNIQUE REFERENCES derivation (id) ON DELETE CASCADE,
                status INTEGER NOT NULL DEFAULT 0,
                substitutable BOOLEAN NOT NULL DEFAULT FALSE,
                substituted BOOLEAN NOT NULL DEFAULT FALSE,
                attempt INTEGER NOT NULL DEFAULT 0,
                timeout_secs BIGINT NULL,
                max_silent_secs BIGINT NULL,
                prefer_local_build BOOLEAN NOT NULL DEFAULT FALSE,
                created_at TIMESTAMP NOT NULL,
                updated_at TIMESTAMP NOT NULL,
                queued_at TIMESTAMP NULL,
                ready_at TIMESTAMP NULL,
                dispatched_at TIMESTAMP NULL
            )
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            INSERT INTO derivation_build
                (id, derivation, status, substitutable, substituted, attempt, created_at, updated_at)
            SELECT uuidv7(), d.id, 0, false, false, 0,
                   (now() AT TIME ZONE 'UTC'), (now() AT TIME ZONE 'UTC')
            FROM derivation d
            ON CONFLICT (derivation) DO NOTHING
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(r#"DROP TABLE IF EXISTS derivation_build"#)
            .await?;

        Ok(())
    }
}
