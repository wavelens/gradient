/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"WITH counts AS (
    SELECT e.derivation, count(*)::int AS n FROM derivation_dependency e
    LEFT JOIN derivation_build dep ON dep.derivation = e.dependency
    WHERE e.kind IN (0, 2) AND (dep.derivation IS NULL OR NOT dep.fetchable)
    GROUP BY e.derivation)
UPDATE derivation_build db SET blocking_deps = coalesce(c.n, 0)
FROM derivation_build p LEFT JOIN counts c ON c.derivation = p.derivation
WHERE db.id = p.id AND p.blocking_deps <> coalesce(c.n, 0)"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
