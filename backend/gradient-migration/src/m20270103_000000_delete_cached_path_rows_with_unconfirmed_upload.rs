/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

// The confirmed column and its partial index stay during the rolling upgrade for servers built from
// main before this change. Servers of this version insert without a value and get the default
// true. Both go away in the next release.
const UP: &str = r#"
SET LOCAL lock_timeout = '5s';
DO $$
DECLARE
    deleted_hashes text[];
    owners uuid[];
    complete uuid[];
BEGIN
    WITH deleted AS (DELETE FROM public.cached_path WHERE NOT confirmed RETURNING hash)
    SELECT coalesce(array_agg(DISTINCT hash), '{}') INTO deleted_hashes FROM deleted;
    SELECT coalesce(array_agg(id ORDER BY id), '{}') INTO owners FROM (
        SELECT id FROM public.derivation WHERE hash = ANY(deleted_hashes)
        UNION SELECT derivation FROM public.derivation_input_source WHERE hash = ANY(deleted_hashes)) o;
    PERFORM 1 FROM public.derivation WHERE id = ANY(owners) ORDER BY id FOR NO KEY UPDATE;
    SELECT coalesce(array_agg(id ORDER BY id), '{}') INTO complete
      FROM public.derivation WHERE id = ANY(owners) AND walked AND unwalked_inputs = 0;
    UPDATE public.derivation SET walked = false WHERE id = ANY(owners) AND walked;
    PERFORM public.ripple_unwalked_inputs(complete, false);
END $$;
"#;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager.get_connection().execute_unprepared(UP).await?;
        Ok(())
    }

    async fn down(&self, _manager: &SchemaManager) -> Result<(), DbErr> {
        Ok(())
    }
}
