/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // BuildStatus: 3=Completed 7=Substituted 4=FailedPermanent 9=FailedTimeout 5=Aborted 6=DependencyFailed.
        // AttemptOutcome: 0=Running 1=Built 2=Substituted 3=Failed 4=Aborted.
        // AttemptFailureReason: 5=BuilderNonzero(for permanent) 6=WallClockTimeout(for timeout).
        let sql = r#"
            INSERT INTO build_attempt
              (id, build, dispatched_job, substitute, outcome, reason,
               failure_message, log_id, build_context, build_time_ms,
               build_started_at, build_finished_at, created_at)
            SELECT
              gen_random_uuid(), b.id, dj.id, false,
              CASE b.status WHEN 3 THEN 1 WHEN 7 THEN 2 WHEN 4 THEN 3 WHEN 9 THEN 3 WHEN 6 THEN 3 WHEN 5 THEN 4 ELSE 0 END,
              CASE b.status WHEN 4 THEN 5 WHEN 9 THEN 6 ELSE NULL END,
              NULL, b.log_id, '{}'::jsonb, b.build_time_ms,
              b.build_started_at, b.build_finished_at, COALESCE(b.updated_at, now())
            FROM build b
            JOIN dispatched_job dj ON dj.build_id = b.id;
        "#;
        manager.get_connection().execute_unprepared(sql).await.map(|_| ())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared("DELETE FROM build_attempt;")
            .await
            .map(|_| ())
    }
}
