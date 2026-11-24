/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CacheId, ProjectId, TeamId, UserId, WebhookId};
pub use gradient_api_types::WebhookScope;

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "webhook")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: WebhookId,
    pub scope: WebhookScope,
    pub project: Option<ProjectId>,
    pub cache: Option<CacheId>,
    pub team: Option<TeamId>,
    pub name: String,
    pub url: String,
    pub secret: String,
    pub events: Json,
    pub active: bool,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
    pub last_fired_at: Option<NaiveDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::webhook_delivery::Entity")]
    Deliveries,
}

impl Related<super::webhook_delivery::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Deliveries.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for gradient_api_types::WebhookResponse {
    fn from(m: Model) -> Self {
        Self {
            last_delivery: None,
            id: m.id,
            scope: m.scope,
            name: m.name,
            url: m.url,
            events: serde_json::from_value(m.events).unwrap_or_default(),
            active: m.active,
            last_fired_at: m.last_fired_at,
            created_by: m.created_by,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}
