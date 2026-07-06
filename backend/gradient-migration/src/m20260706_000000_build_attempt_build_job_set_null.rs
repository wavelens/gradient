/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A `build_attempt` (and its log) outlives the evaluation that drove it: its
//! real owner is the global `derivation_build` anchor (build-once), not the
//! per-eval `build_job`. Under the old `ON DELETE CASCADE` an eval GC deleted
//! the attempt and its log, so a `Completed` anchor reused by a later eval had
//! no retrievable log. Switch the FK to `ON DELETE SET NULL`: eval GC now
//! orphans the attempt from its build_job while it stays attached to the
//! surviving anchor; the attempt (and log) die only when the derivation GC
//! reclaims that anchor.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE build_attempt DROP CONSTRAINT IF EXISTS build_attempt_build_job_fkey;
                ALTER TABLE build_attempt ALTER COLUMN build_job DROP NOT NULL;
                ALTER TABLE build_attempt
                  ADD CONSTRAINT build_attempt_build_job_fkey
                  FOREIGN KEY (build_job) REFERENCES build_job (id) ON DELETE SET NULL;
                "#,
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                r#"
                ALTER TABLE build_attempt DROP CONSTRAINT IF EXISTS build_attempt_build_job_fkey;
                DELETE FROM build_attempt WHERE build_job IS NULL;
                ALTER TABLE build_attempt ALTER COLUMN build_job SET NOT NULL;
                ALTER TABLE build_attempt
                  ADD CONSTRAINT build_attempt_build_job_fkey
                  FOREIGN KEY (build_job) REFERENCES build_job (id) ON DELETE CASCADE;
                "#,
            )
            .await?;

        Ok(())
    }
}
