use sea_orm::entity::prelude::*;
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use chrono::NaiveDateTime;

#[derive(Debug, Clone, PartialEq, Eq, DeriveActiveEnum, EnumIter, Deserialize, Serialize)]
#[sea_orm(rs_type = "i16", db_type = "Integer")]
pub enum BuildStatus {
    #[sea_orm(num_value = 0)]
    Queued,
    #[sea_orm(num_value = 1)]
    Evaluating,
    #[sea_orm(num_value = 2)]
    Building,
    #[sea_orm(num_value = 3)]
    Completed,
    #[sea_orm(num_value = 4)]
    Failed,
    #[sea_orm(num_value = 5)]
    Aborted,
}


#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub project: Uuid,
    pub status: BuildStatus,
    pub path: String,
    pub dependencies: Vec<Uuid>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id"
    )]
    Project,
    #[sea_orm(
        belongs_to = "Entity",
        from = "Column::Dependencies",
        to = "Column::Id"
    )]
    Dependency,
}

impl ActiveModelBehavior for ActiveModel {}
