use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;


#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Server::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Server::Id)
                            .uuid()
                            .not_null()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Server::Organization)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Server::Host)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Server::Port)
                            .integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Server::Architectures)
                            .json()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Server::Features)
                            .json()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Server::LastConnectionAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Server::CreatedBy)
                            .uuid()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(Server::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-servers-organizations")
                            .from(Server::Table, Server::Organization)
                            .to(Organization::Table, Organization::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk-servers-created_by")
                            .from(Server::Table, Server::CreatedBy)
                            .to(User::Table, User::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Server::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Server {
    Table,
    Id,
    Organization,
    Host,
    Port,
    Architectures,
    Features,
    LastConnectionAt,
    CreatedBy,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Organization {
    Table,
    Id,
}

#[derive(DeriveIden)]
enum User {
    Table,
    Id,
}
