use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Organization::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Organization::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Organization::Name)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Organization::Description)
                            .text()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Organization::CreatedBy)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Organization::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-organizations-created_by")
                            .from(Organization::Table, Organization::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Organization::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
    Name,
    Description,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
