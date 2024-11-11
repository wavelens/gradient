use sea_orm::entity::prelude::*;
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use chrono::NaiveDateTime;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "project")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub organization: Uuid,
    pub name: String,
    #[sea_orm(column_type = "Text")]
    pub description: String,
    pub repository: String,
    pub last_evaluation: Option<Uuid>,
    pub last_check_at: NaiveDateTime,
    pub created_by: Uuid,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::organization::Entity",
        from = "Column::Organization",
        to = "super::organization::Column::Id"
    )]
    Organization,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::LastEvaluation",
        to = "super::evaluation::Column::Id"
    )]
    LastEvaluation,
}

impl ActiveModelBehavior for ActiveModel {}
