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
//! There is deliberately no backfill. `walked` seeds false everywhere and the
//! graph is re-derived, because the only value to seed it from is
//! `edges_complete`, the flag being retired for being wrong, and `walked` is
//! monotonic with exactly one writer (`WALKED_UPSERT`, reached only through an
//! evaluation's batch ingest), so an inherited error is permanent and no sweep
//! would ever repair it. Seeding false trades a rebuild for a graph that is
//! true.
//!
//! Re-derivation needs an evaluation, so this requeues the active ones. The
//! four parks left alone are owned elsewhere: `approval` IS the fork-PR
//! approval gate and requeueing it would bypass it, and `no_cache`,
//! `cache_storage_full` and `aborting` have their own hooks. A legacy
//! `waiting_reason` carries no `kind` key and means `workers`, hence the
//! coalesce. Targets `Queued` rather than `Waiting`: this runs before the
//! scheduler starts, and a `Building` evaluation parked to `Waiting` would go
//! straight back to `Building` without ever re-evaluating.
//!
//! Cost at first start: one full evaluation per active row plus a full closure
//! re-walk each, unpruned, in one burst, since the worker-side prune requires
//! `walked`. Dropping the two columns makes this a stop-migrate-start deploy
//! rather than a rolling one: an old server process still selecting
//! `derivation_build.edges_complete` fails the moment the column goes. `DOWN`
//! seeds `edges_complete` from an all-false `walked`, so a rollback inherits an
//! ungated graph and needs the same re-evaluation.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "ALTER TABLE derivation ADD COLUMN IF NOT EXISTS walked boolean NOT NULL DEFAULT false",
    "UPDATE evaluation SET status = 0, waiting_reason = NULL, \
     updated_at = (now() AT TIME ZONE 'UTC') \
     WHERE status IN (1, 2, 3, 8) \
        OR (status = 4 AND coalesce(waiting_reason->>'kind', 'workers') \
            NOT IN ('approval', 'no_cache', 'cache_storage_full', 'aborting'))",
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
