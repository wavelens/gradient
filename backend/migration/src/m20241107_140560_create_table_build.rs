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
                        ColumnDef::new(Build::DerivationPath)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Build::Architecture)
                            .small_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Build::Features)
                            .array(ColumnType::Char(Some(255)))
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Build::ByServer)
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
                            .name("fk-builds-servers")
                            .from(Build::Table, Build::ByServer)
                            .to(Server::Table, Server::Id)
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
    DerivationPath,
    Architecture,
    Features,
    ByServer,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Evaluation {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum Server {
    Table,
    Id,
}