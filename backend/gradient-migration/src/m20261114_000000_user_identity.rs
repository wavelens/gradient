/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = "CREATE TABLE public.user_identity ( \
        id uuid PRIMARY KEY, \
        \"user\" uuid NOT NULL REFERENCES public.\"user\"(id) ON DELETE CASCADE, \
        issuer character varying NOT NULL, \
        subject character varying NOT NULL, \
        created_at timestamp without time zone NOT NULL, \
        CONSTRAINT user_identity_issuer_subject_key UNIQUE (issuer, subject), \
        CONSTRAINT user_identity_user_issuer_key UNIQUE (\"user\", issuer)); \
    INSERT INTO public.user_identity (id, \"user\", issuer, subject, created_at) \
    SELECT gen_random_uuid(), id, oidc_issuer, oidc_subject, created_at FROM public.\"user\" \
    WHERE oidc_issuer IS NOT NULL AND oidc_subject IS NOT NULL; \
    ALTER TABLE public.\"user\" DROP COLUMN oidc_issuer, DROP COLUMN oidc_subject;";

const DOWN: &str = "ALTER TABLE public.\"user\" ADD COLUMN oidc_issuer character varying, \
        ADD COLUMN oidc_subject character varying; \
    UPDATE public.\"user\" u SET oidc_issuer = i.issuer, oidc_subject = i.subject \
    FROM (SELECT DISTINCT ON (\"user\") \"user\", issuer, subject FROM public.user_identity \
        ORDER BY \"user\", created_at) i WHERE i.\"user\" = u.id; \
    DROP TABLE public.user_identity;";

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
