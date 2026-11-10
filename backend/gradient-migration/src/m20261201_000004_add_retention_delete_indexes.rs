/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const DROP: [&str; 2] = [
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-phase_event-at\";",
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-metric_rollup-granularity-bucket_start\";",
];

const CREATE: [&str; 2] = [
    "CREATE INDEX CONCURRENTLY \"idx-phase_event-at\" ON public.phase_event USING btree (at);",
    "CREATE INDEX CONCURRENTLY \"idx-metric_rollup-granularity-bucket_start\" \
        ON public.metric_rollup USING btree (granularity, bucket_start);",
];

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for statement in DROP.into_iter().chain(CREATE) {
            db.execute_unprepared(statement).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        for statement in DROP {
            db.execute_unprepared(statement).await?;
        }
        Ok(())
    }
}
