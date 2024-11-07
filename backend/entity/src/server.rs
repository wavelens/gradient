use sea_orm::entity::prelude::*;
use uuid::Uuid;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize)]
#[sea_orm(rs_type = "i16", db_type = "Integer")]
pub enum Architecture {
    #[sea_orm(num_value = 0)]
    X86_64Linux,
    #[sea_orm(num_value = 1)]
    Aarch64Linux,
    #[sea_orm(num_value = 2)]
    X86_64Darwin,
    #[sea_orm(num_value = 3)]
    Aarch64Darwin,
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "server")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub organization: Uuid,
    pub host: String,
    pub port: i32,
    pub architectures: Vec<Architecture>,
    pub features: Vec<String>,
    pub last_connection_at: DateTimeUtc,
    pub created_by: Uuid,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
}

impl ActiveModelBehavior for ActiveModel {}
