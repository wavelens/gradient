/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Twins (derivations producing the same output paths) were linked by runtime
//! edges, largely by the `m20260927_000002` backfill: each twin's reference to the
//! shared path, its own output, became an edge to the other twin, and the pair's
//! wholeness waited on itself forever. The writers no longer record them; this drops
//! the ones history holds, and the consistency sweep recounts the wholeness they held.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"DELETE FROM derivation_dependency e
     WHERE e.kind = 1
       AND EXISTS (SELECT 1 FROM derivation_output own
                   JOIN derivation_output twin ON twin.hash = own.hash
                   WHERE own.derivation = e.derivation AND twin.derivation = e.dependency)"#,
    r#"UPDATE derivation_dependency e SET kind = 0
     WHERE e.kind = 2
       AND EXISTS (SELECT 1 FROM derivation_output own
                   JOIN derivation_output twin ON twin.hash = own.hash
                   WHERE own.derivation = e.derivation AND twin.derivation = e.dependency)"#,
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

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
