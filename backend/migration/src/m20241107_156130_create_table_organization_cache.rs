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
                    .table(OrganizationCache::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OrganizationCache::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OrganizationCache::Organization)
                            .uuid()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OrganizationCache::Cache).uuid().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-organization_cache-organization")
                            .from(OrganizationCache::Table, OrganizationCache::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-organization_cache-cache")
                            .from(OrganizationCache::Table, OrganizationCache::Cache)
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(OrganizationCache::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum OrganizationCache {
    Table,
    Id,
    Organization,
    Cache,
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}
