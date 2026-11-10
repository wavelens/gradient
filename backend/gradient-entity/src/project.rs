/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;

use crate::ids::{ProjectId, TeamId, UserId};

#[derive(Clone, Default, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "project")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ProjectId,
    #[sea_orm(unique, indexed)]
    pub name: String,
    pub display_name: String,
    #[sea_orm(column_type = "Text")]
    pub description: String,
    pub public_key: String,
    pub private_key: String,
    pub public: bool,
    pub hide_build_requests: bool,
    pub allow_exec_in_build_requests: bool,
    pub restrict_evaluation_fetches: bool,
    pub extra_allowed_uri_prefixes: Vec<String>,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
    pub managed: bool,
    pub owner_team: Option<TeamId>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Project")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("display_name", &self.display_name)
            .field("description", &self.description)
            .field("public_key", &format!("{} {}", self.public_key, self.id))
            .field("private_key", &"[redacted]")
            .field("public", &self.public)
            .field("hide_build_requests", &self.hide_build_requests)
            .field(
                "allow_exec_in_build_requests",
                &self.allow_exec_in_build_requests,
            )
            .field(
                "restrict_evaluation_fetches",
                &self.restrict_evaluation_fetches,
            )
            .field(
                "extra_allowed_uri_prefixes",
                &self.extra_allowed_uri_prefixes,
            )
            .field("created_by", &self.created_by)
            .field("created_at", &self.created_at)
            .field("owner_team", &self.owner_team)
            .finish()
    }
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
