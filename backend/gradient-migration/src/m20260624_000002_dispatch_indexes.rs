/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The `(derivation, dependency)` pair index cannot serve a `dependency`-only filter. The
//! dependent-direction lookups need their own index.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UP: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_build-dispatch-ready"
       ON derivation_build (updated_at)
       WHERE status = 1 AND edges_complete"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_build-promote-ready"
       ON derivation_build (derivation)
       WHERE status = 0 AND edges_complete"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_dependency-dependency"
       ON derivation_dependency (dependency)"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-derivation_build-dispatch-ready""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_build-promote-ready""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_dependency-dependency""#,
];

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for sql in UP {
            db.execute_unprepared(sql).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for sql in DOWN {
            db.execute_unprepared(sql).await?;
        }

        Ok(())
    }
}
