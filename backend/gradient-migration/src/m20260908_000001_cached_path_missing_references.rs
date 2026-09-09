/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `cached_path.missing_references` replaces `closure_complete`: the number of
//! a path's references that are absent, unbacked or themselves not whole. The
//! old flag is converged one last time so the seed reads a sound value, then
//! dropped. On a large cache this runs for minutes and blocks the first start.
//!
//! Runs exactly once. The `IF NOT EXISTS` / `IF EXISTS` guards are for a run that
//! died partway through, not for a re-run: a second `up` finds `closure_complete`
//! already dropped and `converge()` fails on the missing column. The seed skips
//! every row with no reference at all, so the transaction that then takes ACCESS
//! EXCLUSIVE for the `DROP COLUMN` does not first rewrite the whole table. The
//! partial index over the rows below zero keeps the consistency sweep's
//! negative-counter count off a full scan; it is empty on a healthy cache.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const GATE: &str = r#"
    cp.file_hash IS NOT NULL
    AND NOT EXISTS (
        SELECT 1 FROM cached_path_reference r
        LEFT JOIN cached_path dep ON dep.hash = r.reference_hash
        WHERE r.referrer = cp.hash
          AND r.reference_hash <> cp.hash
          AND (dep.hash IS NULL OR dep.file_hash IS NULL OR NOT dep.closure_complete))
"#;

fn converge() -> String {
    format!(
        r#"
        DO $$
        DECLARE changed integer;
        BEGIN
          LOOP
            UPDATE cached_path cp SET closure_complete = false
            WHERE cp.closure_complete AND NOT ({GATE});
            GET DIAGNOSTICS changed = ROW_COUNT;
            EXIT WHEN changed = 0;
          END LOOP;
          LOOP
            UPDATE cached_path cp SET closure_complete = true
            WHERE NOT cp.closure_complete AND ({GATE});
            GET DIAGNOSTICS changed = ROW_COUNT;
            EXIT WHEN changed = 0;
          END LOOP;
        END $$;
        "#
    )
}

const SEED: &str = r#"
    UPDATE cached_path cp SET missing_references = (
        SELECT count(*) FROM cached_path_reference r
        LEFT JOIN cached_path dep ON dep.hash = r.reference_hash
        WHERE r.referrer = cp.hash
          AND r.reference_hash <> cp.hash
          AND NOT (dep.file_hash IS NOT NULL AND dep.closure_complete))
    WHERE EXISTS (SELECT 1 FROM cached_path_reference r WHERE r.referrer = cp.hash)
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(&converge()).await?;
        conn.execute_unprepared(
            "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS missing_references integer NOT NULL DEFAULT 0",
        )
        .await?;
        conn.execute_unprepared(SEED).await?;
        for stmt in [
            "DROP INDEX IF EXISTS \"idx-cached_path-closure_complete\"",
            "DROP INDEX IF EXISTS \"idx-cached_path-closure_pending\"",
            "ALTER TABLE cached_path DROP COLUMN IF EXISTS closure_complete",
            "CREATE INDEX IF NOT EXISTS \"idx-cached_path-negative_references\" ON cached_path (hash) WHERE missing_references < 0",
        ] {
            conn.execute_unprepared(stmt).await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        for stmt in [
            "ALTER TABLE cached_path ADD COLUMN IF NOT EXISTS closure_complete boolean NOT NULL DEFAULT false",
            "UPDATE cached_path SET closure_complete = (file_hash IS NOT NULL AND missing_references = 0)",
            "CREATE INDEX IF NOT EXISTS \"idx-cached_path-closure_complete\" ON cached_path (hash) WHERE closure_complete",
            "CREATE INDEX IF NOT EXISTS \"idx-cached_path-closure_pending\" ON cached_path (hash) WHERE NOT closure_complete AND file_hash IS NOT NULL",
            "DROP INDEX IF EXISTS \"idx-cached_path-negative_references\"",
            "ALTER TABLE cached_path DROP COLUMN IF EXISTS missing_references",
        ] {
            conn.execute_unprepared(stmt).await?;
        }

        Ok(())
    }
}
