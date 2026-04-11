/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A build machine is a host reachable over SSH that the server delegates
//! Nix derivation builds to. Renamed from `server` (which was confusing
//! since Gradient itself is also called "the server").

pub use super::server::Architecture;

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_machine")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    #[sea_orm(indexed)]
    pub name: String,
    pub display_name: String,
    pub organization: Uuid,
    pub active: bool,
    pub host: String,
    pub port: i32,
    pub username: String,
    pub last_connection_at: NaiveDateTime,
    pub max_concurrent_builds: i32,
    pub created_by: Uuid,
    pub created_at: NaiveDateTime,
    pub managed: bool,
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
}

impl ActiveModelBehavior for ActiveModel {}
