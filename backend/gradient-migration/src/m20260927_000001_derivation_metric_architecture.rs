/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build history is predicted per `(pname, architecture)`, newest first, so the
//! architecture is carried on the metric row next to the already denormalized
//! `pname` and indexed in lookup order.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"ALTER TABLE derivation_metric ADD COLUMN architecture text"#,
    r#"UPDATE derivation_metric dm SET architecture = d.architecture
     FROM derivation d WHERE d.id = dm.derivation"#,
    r#"ALTER TABLE derivation_metric ALTER COLUMN architecture SET NOT NULL"#,
    r#"DROP INDEX IF EXISTS "idx-derivation_metric-pname-closure_size""#,
    r#"CREATE INDEX "idx-derivation_metric-pname-architecture-created_at"
     ON derivation_metric (pname, architecture, created_at DESC)"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-derivation_metric-pname-architecture-created_at""#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_metric-pname-closure_size"
     ON derivation_metric (pname, closure_size)"#,
    r#"ALTER TABLE derivation_metric DROP COLUMN architecture"#,
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
