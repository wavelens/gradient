/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE public.derivation_metric ADD COLUMN input_pnames text[];
UPDATE public.derivation_metric m SET input_pnames = (
    SELECT coalesce(array_agg(x.pname ORDER BY x.pname), '{}')
    FROM (SELECT DISTINCT coalesce(nullif(d.pname, ''), regexp_replace(d.name, '-[0-9][^-]*$', ''))
                          COLLATE "C" AS pname
          FROM public.derivation_dependency e JOIN public.derivation d ON d.id = e.dependency
          WHERE e.derivation = m.derivation AND e.kind IN (0, 2)) x);
DROP INDEX public."idx-derivation_metric-pname-architecture-created_at";
"#;

const DOWN: &str = r#"
CREATE INDEX "idx-derivation_metric-pname-architecture-created_at" ON public.derivation_metric USING btree (pname, architecture, created_at DESC);
ALTER TABLE public.derivation_metric DROP COLUMN input_pnames;
"#;

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
