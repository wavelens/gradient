/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
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
                    .table(BuildFeature::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(BuildFeature::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(BuildFeature::Build).uuid().not_null())
                    .col(ColumnDef::new(BuildFeature::Feature).uuid().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_feature-build")
                            .from(BuildFeature::Table, BuildFeature::Build)
                            .to(Build::Table, Build::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-build_feature-feature")
                            .from(BuildFeature::Table, BuildFeature::Feature)
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
            .drop_table(Table::drop().table(BuildFeature::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum BuildFeature {
    Table,
    Id,
    Build,
    Feature,
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Feature {
    Table,
    Id,
}
