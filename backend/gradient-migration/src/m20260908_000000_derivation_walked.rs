/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `derivation.walked`: the derivation's full record (outputs, every declared
//! edge, input sources) is in. Replaces `derivation_build.edges_complete` and
//! `edges_unresolved`: a stub row now exists for every named dependency, so an
//! edge is never deferred and never unresolvable.
//!
//! The backfill is one unbatched full-table `UPDATE derivation` followed by
//! three non-concurrent `CREATE INDEX`, so it holds startup for as long as a
//! production-sized graph takes. Dropping the two columns also makes this a
//! stop-migrate-start deploy rather than a rolling one: an old server process
//! still selecting `derivation_build.edges_complete` fails the moment the
//! column goes.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE derivation ADD COLUMN IF NOT EXISTS walked boolean NOT NULL DEFAULT false",
    "UPDATE derivation d SET walked = true FROM derivation_build db \
     WHERE db.derivation = d.id AND db.edges_complete AND NOT db.edges_unresolved",
    "DROP INDEX IF EXISTS \"idx-derivation_build-dispatch-ready\"",
    "DROP INDEX IF EXISTS \"idx-derivation_build-promote-ready\"",
    "DROP INDEX IF EXISTS \"idx-derivation_build-drv_closure_pending\"",
    "ALTER TABLE derivation_build DROP COLUMN IF EXISTS edges_complete",
    "ALTER TABLE derivation_build DROP COLUMN IF EXISTS edges_unresolved",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-drv_closure_pending\" \
     ON derivation_build (derivation) WHERE NOT drv_closure_cached",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-dispatch-ready\" \
     ON derivation_build (updated_at) WHERE status = 1",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-promote-ready\" \
     ON derivation_build (derivation) WHERE status = 0",
];

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-derivation_build-promote-ready\"",
    "DROP INDEX IF EXISTS \"idx-derivation_build-dispatch-ready\"",
    "DROP INDEX IF EXISTS \"idx-derivation_build-drv_closure_pending\"",
    "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS edges_complete boolean NOT NULL DEFAULT false",
    "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS edges_unresolved boolean NOT NULL DEFAULT false",
    "UPDATE derivation_build db SET edges_complete = d.walked FROM derivation d WHERE d.id = db.derivation",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-drv_closure_pending\" \
     ON derivation_build (derivation) WHERE NOT drv_closure_cached AND edges_complete",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-dispatch-ready\" \
     ON derivation_build (updated_at) WHERE status = 1 AND edges_complete",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-promote-ready\" \
     ON derivation_build (derivation) WHERE status = 0 AND edges_complete",
    "ALTER TABLE derivation DROP COLUMN IF EXISTS walked",
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
