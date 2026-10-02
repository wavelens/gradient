/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    r#"CREATE INDEX IF NOT EXISTS "idx-dispatched_job-created_at"
     ON dispatched_job (created_at)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-worker_connection-disconnected_at"
     ON worker_connection (disconnected_at) WHERE disconnected_at IS NOT NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-webhook_delivery-delivered_at"
     ON webhook_delivery (delivered_at)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-task_action_delivery-delivered_at"
     ON task_action_delivery (delivered_at)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-session-expires_at"
     ON session (expires_at)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-admin_task-kind-finished_at"
     ON admin_task (kind, finished_at) WHERE finished_at IS NOT NULL"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-cache_metric-bucket_time"
     ON cache_metric (bucket_time)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-upstream_metric-bucket_time"
     ON upstream_metric (bucket_time)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-audit_log-created_at"
     ON audit_log (created_at)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-derivation_metric-created_at"
     ON derivation_metric (created_at)"#,
    r#"CREATE INDEX IF NOT EXISTS "idx-cluster_job-finished-updated_at"
     ON cluster_job (updated_at) WHERE status IN (2, 3, 4)"#,
];

const DOWN: &[&str] = &[
    r#"DROP INDEX IF EXISTS "idx-cluster_job-finished-updated_at""#,
    r#"DROP INDEX IF EXISTS "idx-derivation_metric-created_at""#,
    r#"DROP INDEX IF EXISTS "idx-audit_log-created_at""#,
    r#"DROP INDEX IF EXISTS "idx-upstream_metric-bucket_time""#,
    r#"DROP INDEX IF EXISTS "idx-cache_metric-bucket_time""#,
    r#"DROP INDEX IF EXISTS "idx-admin_task-kind-finished_at""#,
    r#"DROP INDEX IF EXISTS "idx-session-expires_at""#,
    r#"DROP INDEX IF EXISTS "idx-task_action_delivery-delivered_at""#,
    r#"DROP INDEX IF EXISTS "idx-webhook_delivery-delivered_at""#,
    r#"DROP INDEX IF EXISTS "idx-worker_connection-disconnected_at""#,
    r#"DROP INDEX IF EXISTS "idx-dispatched_job-created_at""#,
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
