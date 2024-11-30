use sea_orm::entity::prelude::*;
use uuid::Uuid;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "server_architecture")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub server: Uuid,
    pub architecture: super::server::Architecture,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::server::Entity",
        from = "Column::Server",
        to = "super::server::Column::Id"
    )]
    Server,
}

impl ActiveModelBehavior for ActiveModel {}
