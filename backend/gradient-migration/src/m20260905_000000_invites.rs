/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ProjectInvitation::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ProjectInvitation::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ProjectInvitation::Project).uuid().not_null())
                    .col(ColumnDef::new(ProjectInvitation::User).uuid().not_null())
                    .col(ColumnDef::new(ProjectInvitation::Role).uuid().not_null())
                    .col(
                        ColumnDef::new(ProjectInvitation::InvitedBy)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectInvitation::Token)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(ProjectInvitation::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ProjectInvitation::ExpiresAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_invitation-project")
                            .from(ProjectInvitation::Table, ProjectInvitation::Project)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_invitation-user")
                            .from(ProjectInvitation::Table, ProjectInvitation::User)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_invitation-role")
                            .from(ProjectInvitation::Table, ProjectInvitation::Role)
                            .to(Role::Table, Role::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-project_invitation-invited_by")
                            .from(ProjectInvitation::Table, ProjectInvitation::InvitedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-project_invitation-project-user")
                    .table(ProjectInvitation::Table)
                    .col(ProjectInvitation::Project)
                    .col(ProjectInvitation::User)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(CacheInvitation::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CacheInvitation::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(CacheInvitation::Cache).uuid().not_null())
                    .col(ColumnDef::new(CacheInvitation::User).uuid().not_null())
                    .col(ColumnDef::new(CacheInvitation::Role).uuid().not_null())
                    .col(ColumnDef::new(CacheInvitation::InvitedBy).uuid().not_null())
                    .col(
                        ColumnDef::new(CacheInvitation::Token)
                            .text()
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(CacheInvitation::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CacheInvitation::ExpiresAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_invitation-cache")
                            .from(CacheInvitation::Table, CacheInvitation::Cache)
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_invitation-user")
                            .from(CacheInvitation::Table, CacheInvitation::User)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_invitation-role")
                            .from(CacheInvitation::Table, CacheInvitation::Role)
                            .to(CacheRole::Table, CacheRole::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_invitation-invited_by")
                            .from(CacheInvitation::Table, CacheInvitation::InvitedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-cache_invitation-cache-user")
                    .table(CacheInvitation::Table)
                    .col(CacheInvitation::Cache)
                    .col(CacheInvitation::User)
                    .unique()
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(CacheSubscriptionRequest::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CacheSubscriptionRequest::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(CacheSubscriptionRequest::Project)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CacheSubscriptionRequest::Cache)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CacheSubscriptionRequest::Mode)
                            .integer()
                            .not_null()
                            .default(0),
                    )
                    .col(
                        ColumnDef::new(CacheSubscriptionRequest::RequestedBy)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CacheSubscriptionRequest::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_subscription_request-project")
                            .from(
                                CacheSubscriptionRequest::Table,
                                CacheSubscriptionRequest::Project,
                            )
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_subscription_request-cache")
                            .from(
                                CacheSubscriptionRequest::Table,
                                CacheSubscriptionRequest::Cache,
                            )
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_subscription_request-requested_by")
                            .from(
                                CacheSubscriptionRequest::Table,
                                CacheSubscriptionRequest::RequestedBy,
                            )
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .name("idx-cache_subscription_request-project-cache")
                    .table(CacheSubscriptionRequest::Table)
                    .col(CacheSubscriptionRequest::Project)
                    .col(CacheSubscriptionRequest::Cache)
                    .unique()
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(CacheSubscriptionRequest::Table)
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(Table::drop().table(CacheInvitation::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(ProjectInvitation::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ProjectInvitation {
    Table,
    Id,
    Project,
    User,
    Role,
    InvitedBy,
    Token,
    CreatedAt,
    ExpiresAt,
}

#[derive(DeriveIden)]
enum CacheInvitation {
    Table,
    Id,
    Cache,
    User,
    Role,
    InvitedBy,
    Token,
    CreatedAt,
    ExpiresAt,
}

#[derive(DeriveIden)]
enum CacheSubscriptionRequest {
    Table,
    Id,
    Project,
    Cache,
    Mode,
    RequestedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum CacheRole {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Role {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
