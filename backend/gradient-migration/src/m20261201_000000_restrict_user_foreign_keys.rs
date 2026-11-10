/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const UP: &str = r#"
ALTER TABLE ONLY public.cache
    DROP CONSTRAINT "fk-cache-created_by",
    ADD CONSTRAINT "fk-cache-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE NO ACTION;
ALTER TABLE ONLY public.integration
    DROP CONSTRAINT "fk-integration-created_by",
    ADD CONSTRAINT "fk-integration-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE NO ACTION;
ALTER TABLE ONLY public.project
    DROP CONSTRAINT "fk-project-created_by",
    ADD CONSTRAINT "fk-project-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE NO ACTION;
ALTER TABLE ONLY public.task
    DROP CONSTRAINT "fk-task-created_by",
    ADD CONSTRAINT "fk-task-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE NO ACTION;
ALTER TABLE ONLY public.worker_registration
    DROP CONSTRAINT "fk-worker_registration-created_by",
    ADD CONSTRAINT "fk-worker_registration-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE SET NULL;
ALTER TABLE ONLY public.project_user
    DROP CONSTRAINT "fk-project_user-role",
    ADD CONSTRAINT "fk-project_user-role" FOREIGN KEY (role) REFERENCES public.role(id) ON UPDATE CASCADE ON DELETE NO ACTION;
ALTER TABLE ONLY public.cache_user
    DROP CONSTRAINT "fk-cache_user-role",
    ADD CONSTRAINT "fk-cache_user-role" FOREIGN KEY (role) REFERENCES public.cache_role(id) ON UPDATE CASCADE ON DELETE NO ACTION;
ALTER TABLE ONLY public.team_project
    DROP CONSTRAINT team_project_role_fkey,
    ADD CONSTRAINT team_project_role_fkey FOREIGN KEY (role) REFERENCES public.role(id) ON DELETE NO ACTION;
"#;

const DOWN: &str = r#"
ALTER TABLE ONLY public.cache
    DROP CONSTRAINT "fk-cache-created_by",
    ADD CONSTRAINT "fk-cache-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;
ALTER TABLE ONLY public.integration
    DROP CONSTRAINT "fk-integration-created_by",
    ADD CONSTRAINT "fk-integration-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;
ALTER TABLE ONLY public.project
    DROP CONSTRAINT "fk-project-created_by",
    ADD CONSTRAINT "fk-project-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;
ALTER TABLE ONLY public.task
    DROP CONSTRAINT "fk-task-created_by",
    ADD CONSTRAINT "fk-task-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE CASCADE;
ALTER TABLE ONLY public.worker_registration
    DROP CONSTRAINT "fk-worker_registration-created_by",
    ADD CONSTRAINT "fk-worker_registration-created_by" FOREIGN KEY (created_by) REFERENCES public."user"(id);
ALTER TABLE ONLY public.project_user
    DROP CONSTRAINT "fk-project_user-role",
    ADD CONSTRAINT "fk-project_user-role" FOREIGN KEY (role) REFERENCES public.role(id) ON UPDATE CASCADE ON DELETE CASCADE;
ALTER TABLE ONLY public.cache_user
    DROP CONSTRAINT "fk-cache_user-role",
    ADD CONSTRAINT "fk-cache_user-role" FOREIGN KEY (role) REFERENCES public.cache_role(id) ON UPDATE CASCADE ON DELETE RESTRICT;
ALTER TABLE ONLY public.team_project
    DROP CONSTRAINT team_project_role_fkey,
    ADD CONSTRAINT team_project_role_fkey FOREIGN KEY (role) REFERENCES public.role(id) ON DELETE CASCADE;
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
