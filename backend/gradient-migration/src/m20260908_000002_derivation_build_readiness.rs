/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `derivation_build.fetchable` and `unready_deps` replace `closure_complete`
//! and `drv_closure_cached`. Both are backfilled in two set-based statements
//! from the NAR counter, then `Created`/`Queued` are normalised to the new
//! invariant: `Queued` means the gates held.
//!
//! The normalisation is not a no-op on the first start. `derivation.walked`
//! is seeded false for every row by the migration before this one, and
//! `gates()` requires it, so the demote statement moves every `Queued` anchor
//! back to `Created` and the promote statement promotes nothing.
//!
//! That is intended: the same earlier migration requeues every non-terminal
//! evaluation, and re-walking a graph sets `walked` and re-promotes its anchors
//! through the ordinary readiness path. Seeding from the flags being retired is
//! deliberately not an option, so every value here is re-derived from ground
//! truth: the NAR counter, the anchor's own status, and `build_job`.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const WHOLE: &str = "(cp.file_hash IS NOT NULL AND cp.missing_references = 0)";

fn fetchable() -> String {
    format!(
        "(db.substitutable OR (db.status IN (3, 7) AND NOT EXISTS ( \
           SELECT 1 FROM derivation_output o LEFT JOIN cached_path cp ON cp.hash = o.hash \
           WHERE o.derivation = db.derivation AND NOT {WHOLE})))"
    )
}

fn gates() -> String {
    format!(
        "(EXISTS (SELECT 1 FROM derivation w WHERE w.id = db.derivation AND w.walked) \
          AND db.unready_deps = 0 \
          AND EXISTS (SELECT 1 FROM build_job bj WHERE bj.derivation = db.derivation) \
          AND (db.substitutable OR EXISTS ( \
              SELECT 1 FROM derivation d JOIN cached_path cp ON cp.hash = d.hash \
              WHERE d.id = db.derivation AND {WHOLE})))"
    )
}

fn up_statements() -> Vec<String> {
    vec![
        "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS fetchable boolean NOT NULL DEFAULT false".into(),
        "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS unready_deps integer NOT NULL DEFAULT 0".into(),
        format!("UPDATE derivation_build db SET fetchable = true WHERE {}", fetchable()),
        "UPDATE derivation_build db SET unready_deps = c.n FROM ( \
           SELECT e.derivation, count(*) AS n FROM derivation_dependency e \
           JOIN derivation_build dep ON dep.derivation = e.dependency \
           WHERE NOT dep.fetchable GROUP BY e.derivation) c \
         WHERE db.derivation = c.derivation".into(),
        format!("UPDATE derivation_build db SET status = 0, updated_at = (now() AT TIME ZONE 'UTC') WHERE db.status = 1 AND NOT {}", gates()),
        format!("UPDATE derivation_build db SET status = 1, queued_at = coalesce(db.queued_at, now() AT TIME ZONE 'UTC'), updated_at = (now() AT TIME ZONE 'UTC') WHERE db.status = 0 AND {}", gates()),
        "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-promotable\" ON derivation_build (derivation) WHERE status = 0 AND unready_deps = 0".into(),
    ]
}

const DOWN: &[&str] = &[
    "DROP INDEX IF EXISTS \"idx-derivation_build-promotable\"",
    "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS closure_complete boolean NOT NULL DEFAULT false",
    "ALTER TABLE derivation_build ADD COLUMN IF NOT EXISTS drv_closure_cached boolean NOT NULL DEFAULT false",
    "UPDATE derivation_build SET closure_complete = fetchable AND NOT substitutable",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-promote-ready\" ON derivation_build (derivation) WHERE status = 0",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-closure_complete\" ON derivation_build (derivation) WHERE closure_complete",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-closure_pending\" ON derivation_build (status) WHERE NOT closure_complete",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-drv_closure_cached\" ON derivation_build (derivation) WHERE drv_closure_cached",
    "CREATE INDEX IF NOT EXISTS \"idx-derivation_build-drv_closure_pending\" ON derivation_build (derivation) WHERE NOT drv_closure_cached",
    "ALTER TABLE derivation_build DROP COLUMN IF EXISTS unready_deps",
    "ALTER TABLE derivation_build DROP COLUMN IF EXISTS fetchable",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for stmt in up_statements() {
            manager.get_connection().execute_unprepared(&stmt).await?;
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
