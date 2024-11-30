use sea_orm::entity::prelude::*;
use uuid::Uuid;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "server_feature")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub server: Uuid,
    pub feature: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Server,
    Feature,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Server => Entity::belongs_to(super::server::Entity)
                .from(Column::Server)
                .to(super::server::Column::Id)
                .into(),
            Self::Feature => Entity::belongs_to(super::feature::Entity)
                .from(Column::Feature)
                .to(super::feature::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
