/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Indexes for the scheduler hot paths that previously seq-scanned the global
//! `derivation_build` (one row per derivation): the dispatcher's `status = Queued
//! AND edges_complete` gate and promotion's `status = Created AND edges_complete`
//! gate, plus `derivation_dependency (dependency)` for the dependent-direction
//! lookups in `promote_dependents` and `cascade_dependency_failed` (the existing
//! `(derivation, dependency)` pair cannot serve a `dependency`-only filter).

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
