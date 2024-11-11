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
                        ColumnDef::new(Build::Evaluation)
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
                        ColumnDef::new(Build::DependencyOf)
                            .uuid(),
                    )
                    .col(
                        ColumnDef::new(Build::CreatedAt)
                            .date_time()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-builds-evaluation")
                            .from(Build::Table, Build::Evaluation)
                            .to(Evaluation::Table, Evaluation::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-builds-dependency_of")
                            .from(Build::Table, Build::DependencyOf)
                            .to(Build::Table, Build::Id)
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
    Evaluation,
    Status,
    Path,
    DependencyOf,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
}
