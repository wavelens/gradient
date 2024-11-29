use sea_orm::entity::prelude::*;
use uuid::Uuid;
use serde::{Deserialize, Serialize};
use chrono::NaiveDateTime;

#[derive(Clone, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "api")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    pub owned_by: Uuid,
    pub name: String,
    pub key: String,
    pub last_used_at: NaiveDateTime,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::OwnedBy",
        to = "super::user::Column::Id"
    )]
    OwnedBy,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("User")
            .field("id", &self.id)
            .field("owned_by", &self.owned_by)
            .field("name", &self.name)
            .field("key", &"[redacted]")
            .field("last_used_at", &self.last_used_at)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl ActiveModelBehavior for ActiveModel {}
