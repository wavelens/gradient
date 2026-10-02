/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();

        conn.execute_unprepared(
            "ALTER TABLE cached_path
               ADD COLUMN IF NOT EXISTS closure_complete boolean NOT NULL DEFAULT false",
        )
        .await?;
        conn.execute_unprepared(
            "ALTER TABLE derivation_build
               ADD COLUMN IF NOT EXISTS closure_complete boolean NOT NULL DEFAULT false",
        )
        .await?;

        // A present NAR is marked complete once every non-self reference is present and complete.
        // Leaves are settling first, then their referrers.
        conn.execute_unprepared(
            r#"
            DO $$
            DECLARE changed integer;
            BEGIN
              LOOP
                UPDATE cached_path cp SET closure_complete = true
                WHERE cp.closure_complete = false
                  AND cp.file_hash IS NOT NULL
                  AND NOT EXISTS (
                    SELECT 1
                    FROM regexp_split_to_table(coalesce(cp."references", ''), '\s+') AS tok
                    WHERE tok <> ''
                      AND split_part(tok, '-', 1) <> cp.hash
                      AND NOT EXISTS (
                        SELECT 1 FROM cached_path r
                        WHERE r.hash = split_part(tok, '-', 1)
                          AND r.file_hash IS NOT NULL
                          AND r.closure_complete = true));
                GET DIAGNOSTICS changed = ROW_COUNT;
                EXIT WHEN changed = 0;
              END LOOP;
            END $$;
            "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            UPDATE derivation_build db SET closure_complete = true
            WHERE db.status IN (3, 7)
              AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation)
              AND NOT EXISTS (
                SELECT 1 FROM derivation_output o
                LEFT JOIN cached_path cp ON cp.hash = o.hash
                WHERE o.derivation = db.derivation AND (cp.closure_complete IS NOT TRUE));
            "#,
        )
        .await?;

        conn.execute_unprepared(
            r#"
            UPDATE derivation_build db SET
              status = 0, substituted = false, attempt = 0,
              substitutable = EXISTS (
                SELECT 1 FROM derivation_output o
                WHERE o.derivation = db.derivation AND o.external_url IS NOT NULL),
              updated_at = (now() AT TIME ZONE 'UTC')
            WHERE db.status IN (3, 7)
              AND db.closure_complete = false
              AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation);
            "#,
        )
        .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let conn = manager.get_connection();
        conn.execute_unprepared(
            "ALTER TABLE derivation_build DROP COLUMN IF EXISTS closure_complete",
        )
        .await?;
        conn.execute_unprepared("ALTER TABLE cached_path DROP COLUMN IF EXISTS closure_complete")
            .await?;

        Ok(())
    }
}
