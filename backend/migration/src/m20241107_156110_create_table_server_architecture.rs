use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;


#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(ServerArchitecture::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(ServerArchitecture::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(ServerArchitecture::Server)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(ServerArchitecture::Architecture)
                            .small_integer()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-server_architecture-server")
                            .from(ServerArchitecture::Table, ServerArchitecture::Server)
                            .to(Server::Table, Server::Id)
                            .on_delete(ForeignKeyAction::Cascade)
                            .on_update(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(ServerArchitecture::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum ServerArchitecture {
    Table,
    Id,
    Server,
    Architecture,
}

#[derive(DeriveIden)]
enum Server {
    Table,
    Id,
}

