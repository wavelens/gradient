/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
DROP TRIGGER evaluation_shared_build_moved ON public.derivation_build;
CREATE TRIGGER evaluation_shared_build_moved AFTER UPDATE OF status, wanted ON public.derivation_build REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION public.evaluation_shared_build_moved();
"#;

const DOWN: &str = r#"
DROP TRIGGER evaluation_shared_build_moved ON public.derivation_build;
CREATE TRIGGER evaluation_shared_build_moved AFTER UPDATE ON public.derivation_build REFERENCING OLD TABLE AS old_rows NEW TABLE AS new_rows FOR EACH STATEMENT EXECUTE FUNCTION public.evaluation_shared_build_moved();
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
