/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::{ConnectionTrait, Statement};
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const ADMIN_ID: &str = "00000000-0000-0000-0000-000000000011";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CacheUser::Table)
                    .if_not_exists()
                    .col(ColumnDef::new(CacheUser::Id).uuid().not_null().primary_key())
                    .col(ColumnDef::new(CacheUser::Cache).uuid().not_null())
                    .col(ColumnDef::new(CacheUser::User).uuid().not_null())
                    .col(ColumnDef::new(CacheUser::Role).uuid().not_null())
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_user-cache")
                            .from(CacheUser::Table, CacheUser::Cache)
                            .to(Cache::Table, Cache::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_user-user")
                            .from(CacheUser::Table, CacheUser::User)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-cache_user-role")
                            .from(CacheUser::Table, CacheUser::Role)
                            .to(CacheRole::Table, CacheRole::Id)
                            .on_delete(ForeignKeyAction::Restrict)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .index(
                        Index::create()
                            .name("uq-cache_user-cache-user")
                            .table(CacheUser::Table)
                            .col(CacheUser::Cache)
                            .col(CacheUser::User)
                            .unique(),
                    )
                    .to_owned(),
            )
            .await?;

        let db = manager.get_connection();
        let backend = db.get_database_backend();
        let sql = format!(
            r#"
            INSERT INTO cache_user (id, cache, "user", role)
            SELECT gen_random_uuid(), c.id, c.created_by, '{admin}'::uuid
            FROM cache c
            WHERE NOT EXISTS (
                SELECT 1 FROM cache_user cu
                WHERE cu.cache = c.id AND cu."user" = c.created_by
            );
            "#,
            admin = ADMIN_ID,
        );
        db.execute(Statement::from_string(backend, sql)).await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(CacheUser::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum CacheUser {
    Table,
    Id,
    Cache,
    User,
    Role,
}

#[derive(DeriveIden)]
enum Cache {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum CacheRole {
    Table,
    Id,
}
