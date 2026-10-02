/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-cached_path_signature-pending"
       ON cached_path_signature (id) WHERE signature IS NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-cached_path-closure_complete"
       ON cached_path (hash) WHERE closure_complete"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-cached_path-closure_pending"
       ON cached_path (hash) WHERE NOT closure_complete AND file_hash IS NOT NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_build-closure_complete"
       ON derivation_build (derivation) WHERE closure_complete"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_build-closure_pending"
       ON derivation_build (status) WHERE NOT closure_complete"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_build-drv_closure_cached"
       ON derivation_build (derivation) WHERE drv_closure_cached"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_build-drv_closure_pending"
       ON derivation_build (derivation) WHERE NOT drv_closure_cached AND edges_complete"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-cached_path_signature-pending""#,
    r#"DROP INDEX IF EXISTS "idx-cached_path-closure_complete""#,
    r#"DROP INDEX IF EXISTS "idx-cached_path-closure_pending""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_build-closure_complete""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_build-closure_pending""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_build-drv_closure_cached""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_build-drv_closure_pending""#,
];

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
