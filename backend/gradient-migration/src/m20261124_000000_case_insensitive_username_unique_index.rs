/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// Usernames that differ only by case conflict with the unique index on lower(username). The migration gave the name to the state-managed account first, then to the oldest account. Every other account got the first free numbered suffix (Alice-2). The server can still start. A user with a new name can sign in with email.
const RENAME_CASE_DUPLICATES: &str = r#"
DO $$
DECLARE
    duplicate RECORD;
    suffix INTEGER;
    candidate VARCHAR;
BEGIN
    FOR duplicate IN
        SELECT id, username FROM (
            SELECT id, username, row_number() OVER (
                PARTITION BY lower(username) ORDER BY managed DESC, created_at, id
            ) AS position
            FROM public."user"
        ) ranked
        WHERE position > 1
        ORDER BY lower(username), position
    LOOP
        suffix := 2;
        LOOP
            candidate := rtrim(left(duplicate.username, 49 - length(suffix::text)), '-_')
                || '-' || suffix;
            EXIT WHEN NOT EXISTS (
                SELECT 1 FROM public."user" WHERE lower(username) = lower(candidate)
            );
            suffix := suffix + 1;
        END LOOP;
        UPDATE public."user" SET username = candidate WHERE id = duplicate.id;
        RAISE NOTICE 'renamed user % from % to % because old accounts have the same username apart from case',
            duplicate.id, duplicate.username, candidate;
    END LOOP;
END $$;
"#;

const UP: &str = "ALTER TABLE public.\"user\" DROP CONSTRAINT user_username_key; \
    CREATE UNIQUE INDEX user_username_key ON public.\"user\" (lower(username));";

const DOWN: &str = "DROP INDEX public.user_username_key; \
    ALTER TABLE public.\"user\" ADD CONSTRAINT user_username_key UNIQUE (username);";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        db.execute_unprepared(RENAME_CASE_DUPLICATES).await?;
        db.execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(DOWN).await?;
        Ok(())
    }
}
