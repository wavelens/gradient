/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

use crate::sql_on_one_connection::run_sql_statements_on_one_connection;

const LOCK_TIMEOUT_SQL: &str = "SET lock_timeout = '5s';";

const DROP_WORKER_REGISTRATION: &str =
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-derivation_metric-worker_registration\";";

const DROP_TEAM_WORKER: &str =
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-derivation_metric-team_worker\";";

const DROP_PROJECT: &str =
    "DROP INDEX CONCURRENTLY IF EXISTS public.\"idx-derivation_metric-project\";";

const CREATE_WORKER_REGISTRATION: &str = "CREATE INDEX CONCURRENTLY \
    \"idx-derivation_metric-worker_registration\" \
    ON public.derivation_metric (worker_registration) WHERE worker_registration IS NOT NULL;";

const CREATE_TEAM_WORKER: &str = "CREATE INDEX CONCURRENTLY \
    \"idx-derivation_metric-team_worker\" \
    ON public.derivation_metric (team_worker) WHERE team_worker IS NOT NULL;";

const CREATE_PROJECT: &str = "CREATE INDEX CONCURRENTLY \
    \"idx-derivation_metric-project\" \
    ON public.derivation_metric (project) WHERE project IS NOT NULL;";

const RESET_LOCK_TIMEOUT_SQL: &str = "RESET lock_timeout;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    fn use_transaction(&self) -> Option<bool> {
        Some(false)
    }

    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[
                LOCK_TIMEOUT_SQL,
                DROP_WORKER_REGISTRATION,
                DROP_TEAM_WORKER,
                DROP_PROJECT,
                CREATE_WORKER_REGISTRATION,
                CREATE_TEAM_WORKER,
                CREATE_PROJECT,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        run_sql_statements_on_one_connection(
            manager,
            &[
                LOCK_TIMEOUT_SQL,
                DROP_WORKER_REGISTRATION,
                DROP_TEAM_WORKER,
                DROP_PROJECT,
                RESET_LOCK_TIMEOUT_SQL,
            ],
        )
        .await
    }
}
