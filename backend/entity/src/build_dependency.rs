use sea_orm::entity::prelude::*;
use uuid::Uuid;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_dependency")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub build: Uuid,
    pub dependency: Uuid,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Build,
    Dependency,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Build => Entity::belongs_to(super::build::Entity)
                .from(Column::Build)
                .to(super::build::Column::Id)
                .into(),
            Self::Dependency => Entity::belongs_to(super::build::Entity)
                .from(Column::Dependency)
                .to(super::build::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
