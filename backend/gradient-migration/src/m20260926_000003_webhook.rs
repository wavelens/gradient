/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS webhook ( \
        id uuid PRIMARY KEY, \
        scope smallint NOT NULL, \
        project uuid REFERENCES project(id) ON DELETE CASCADE, \
        cache uuid REFERENCES cache(id) ON DELETE CASCADE, \
        name text NOT NULL, \
        url text NOT NULL, \
        secret text NOT NULL, \
        events jsonb NOT NULL DEFAULT '[]', \
        active boolean NOT NULL DEFAULT true, \
        created_by uuid NOT NULL REFERENCES \"user\"(id), \
        created_at timestamp NOT NULL, \
        updated_at timestamp NOT NULL, \
        last_fired_at timestamp, \
        CONSTRAINT \"chk-webhook-scope\" CHECK ( \
            (scope = 0 AND project IS NOT NULL AND cache IS NULL) OR \
            (scope = 1 AND cache IS NOT NULL AND project IS NULL) OR \
            (scope = 2 AND project IS NULL AND cache IS NULL)))",
    "CREATE UNIQUE INDEX IF NOT EXISTS \"idx-webhook-name\" \
     ON webhook (scope, coalesce(project, cache), name)",
    "CREATE INDEX IF NOT EXISTS \"idx-webhook-project\" ON webhook (project) \
     WHERE active AND project IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS \"idx-webhook-cache\" ON webhook (cache) \
     WHERE active AND cache IS NOT NULL",
    "CREATE TABLE IF NOT EXISTS webhook_delivery ( \
        id uuid PRIMARY KEY, \
        webhook_id uuid NOT NULL REFERENCES webhook(id) ON DELETE CASCADE, \
        event text NOT NULL, \
        request_body text NOT NULL, \
        response_status integer, \
        response_body text, \
        error_message text, \
        success boolean NOT NULL, \
        duration_ms integer NOT NULL, \
        delivered_at timestamp NOT NULL)",
    "CREATE INDEX IF NOT EXISTS \"idx-webhook-delivery-webhook\" \
     ON webhook_delivery (webhook_id, delivered_at DESC)",
];

const DOWN: &[&str] = &[
    "DROP TABLE IF EXISTS webhook_delivery",
    "DROP TABLE IF EXISTS webhook",
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
