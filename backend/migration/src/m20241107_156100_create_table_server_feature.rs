/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
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
                    .table(ServerFeature::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ServerFeature::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(ServerFeature::Server).uuid().not_null())
                    .col(ColumnDef::new(ServerFeature::Feature).uuid().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-server_feature-server")
                            .from(ServerFeature::Table, ServerFeature::Server)
                            .to(Server::Table, Server::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-server_feature-feature")
                            .from(ServerFeature::Table, ServerFeature::Feature)
                            .to(Feature::Table, Feature::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ServerFeature::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ServerFeature {
    Table,
    Id,
    Server,
    Feature,
}

#[derive(DeriveIden)]
enum Server {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Feature {
    Table,
    Id,
}
