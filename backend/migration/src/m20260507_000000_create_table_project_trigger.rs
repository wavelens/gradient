/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Adds the `project_trigger` table that defines how/when evaluations are
//! created for a project (polling, reporter push, reporter PR, time/cron).
//! Seeds one default polling trigger (300s, skip) for every existing project.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ProjectTrigger::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProjectTrigger::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ProjectTrigger::Project).uuid().not_null())
                    .col(
                        ColumnDef::new(ProjectTrigger::TriggerType)
                            .small_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTrigger::Concurrency)
                            .small_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTrigger::Config)
                            .json_binary()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTrigger::Active)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(
                        ColumnDef::new(ProjectTrigger::LastFiredAt)
                            .date_time()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTrigger::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectTrigger::UpdatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_trigger-project")
                            .from(ProjectTrigger::Table, ProjectTrigger::Project)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_project_trigger_project_active")
                    .table(ProjectTrigger::Table)
                    .col(ProjectTrigger::Project)
                    .col(ProjectTrigger::Active)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_project_trigger_type_active")
                    .table(ProjectTrigger::Table)
                    .col(ProjectTrigger::TriggerType)
                    .col(ProjectTrigger::Active)
                    .to_owned(),
            )
            .await?;

        // Seed: one default polling trigger (interval=300s, concurrency=skip) per existing project.
        let db = manager.get_connection();
        db.execute_unprepared(
            r#"
            INSERT INTO project_trigger (id, project, trigger_type, concurrency, config, active, last_fired_at, created_at, updated_at)
            SELECT gen_random_uuid(), p.id, 0, 3, '{"interval_secs":300}'::jsonb, true, NULL, NOW(), NOW()
            FROM project p
            WHERE NOT EXISTS (
                SELECT 1 FROM project_trigger pt WHERE pt.project = p.id AND pt.trigger_type = 0
            )
            "#,
        )
        .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ProjectTrigger::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ProjectTrigger {
    #[sea_orm(iden = "project_trigger")]
    Table,
    Id,
    Project,
    TriggerType,
    Concurrency,
    Config,
    Active,
    LastFiredAt,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
}
