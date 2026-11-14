/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "ALTER TABLE public.task \
    ADD COLUMN restrict_evaluation_fetches boolean DEFAULT false NOT NULL, \
    ADD COLUMN extra_allowed_uri_prefixes text[] DEFAULT '{}' NOT NULL; \
    UPDATE public.task t SET restrict_evaluation_fetches = p.restrict_evaluation_fetches, \
    extra_allowed_uri_prefixes = p.extra_allowed_uri_prefixes \
    FROM public.project p WHERE p.id = t.project \
    AND NOT (t.managed AND t.name = 'build-request'); \
    ALTER TABLE public.project \
    DROP COLUMN extra_allowed_uri_prefixes, \
    DROP COLUMN restrict_evaluation_fetches;";

const DOWN: &str = "ALTER TABLE public.project \
    ADD COLUMN restrict_evaluation_fetches boolean DEFAULT false NOT NULL, \
    ADD COLUMN extra_allowed_uri_prefixes text[] DEFAULT '{}' NOT NULL; \
    UPDATE public.project p SET restrict_evaluation_fetches = true \
    WHERE EXISTS (SELECT 1 FROM public.task t WHERE t.project = p.id AND t.restrict_evaluation_fetches \
    AND NOT (t.managed AND t.name = 'build-request')); \
    UPDATE public.project p SET extra_allowed_uri_prefixes = COALESCE(( \
    SELECT array_agg(DISTINCT prefix ORDER BY prefix) FROM public.task t, \
    unnest(t.extra_allowed_uri_prefixes) AS prefix WHERE t.project = p.id \
    AND NOT (t.managed AND t.name = 'build-request')), '{}'); \
    ALTER TABLE public.task \
    DROP COLUMN extra_allowed_uri_prefixes, \
    DROP COLUMN restrict_evaluation_fetches;";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
