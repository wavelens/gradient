use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Build::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Build::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Build::Project)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Build::Status)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Build::Path)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Build::Dependencies)
                            .json()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Build::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-builds-project")
                            .from(Build::Table, Build::Project)
                            .to(Project::Table, Project::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Build::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Build {
    Table,
    Id,
    Project,
    Status,
    Path,
    Dependencies,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Project {
    Table,
    Id,
}
