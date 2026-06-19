/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build-identity cutover: introduce the per-eval scored `build_job`, re-point
//! `build_attempt` onto `build_job` + the `derivation_build` anchor, point
//! `entry_point` at a derivation, and drop the per-eval `build` table. Per-eval
//! build/attempt rows are reconstructable, so this is a clean cutover; the next
//! evaluation re-resolves anchors and re-creates jobs.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();

        db.execute_unprepared(
            r#"
            CREATE TABLE build_job (
                id UUID PRIMARY KEY,
                evaluation UUID NOT NULL REFERENCES evaluation (id) ON DELETE CASCADE,
                derivation UUID NOT NULL REFERENCES derivation (id),
                derivation_build UUID NOT NULL REFERENCES derivation_build (id) ON DELETE CASCADE,
                score DOUBLE PRECISION NOT NULL DEFAULT 0,
                score_breakdown JSONB NOT NULL DEFAULT '{}'::jsonb,
                created_at TIMESTAMP NOT NULL,
                UNIQUE (evaluation, derivation)
            );
            CREATE INDEX idx_build_job_derivation_build ON build_job (derivation_build);
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            ALTER TABLE entry_point ADD COLUMN derivation UUID;
            UPDATE entry_point ep SET derivation = b.derivation FROM build b WHERE b.id = ep.build;
            DELETE FROM entry_point WHERE derivation IS NULL;
            ALTER TABLE entry_point DROP COLUMN build;
            ALTER TABLE entry_point ALTER COLUMN derivation SET NOT NULL;
            ALTER TABLE entry_point
                ADD CONSTRAINT "fk-entry_point-derivation"
                FOREIGN KEY (derivation) REFERENCES derivation (id);
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            DROP TABLE build_attempt;
            CREATE TABLE build_attempt (
                id UUID PRIMARY KEY,
                build_job UUID NOT NULL REFERENCES build_job (id) ON DELETE CASCADE,
                derivation_build UUID NOT NULL REFERENCES derivation_build (id) ON DELETE CASCADE,
                dispatched_job UUID NOT NULL,
                substitute BOOLEAN NOT NULL DEFAULT FALSE,
                outcome INTEGER NOT NULL DEFAULT 0,
                reason INTEGER NULL,
                failure_message TEXT NULL,
                build_context JSONB NOT NULL DEFAULT '{}'::jsonb,
                build_started_at TIMESTAMP NULL,
                build_finished_at TIMESTAMP NULL,
                created_at TIMESTAMP NOT NULL
            );
            CREATE INDEX idx_build_attempt_derivation_build ON build_attempt (derivation_build);
            CREATE INDEX idx_build_attempt_dispatched_job ON build_attempt (dispatched_job);
            "#,
        )
        .await?;

        db.execute_unprepared(
            r#"
            TRUNCATE build_log_chunk;
            ALTER TABLE build_log_chunk RENAME COLUMN build TO build_attempt;
            "#,
        )
        .await?;

        db.execute_unprepared(r#"DROP TABLE build"#).await?;

        Ok(())
    }

    async fn down(&self, _: &SchemaManager) -> Result<(), DbErr> {
        Err(DbErr::Migration(
            "build_job_and_attempt cutover is irreversible".into(),
        ))
    }
}
