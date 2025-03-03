/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

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

impl std::str::FromStr for Architecture {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "x86_64-linux" => Ok(Architecture::X86_64Linux),
            "aarch64-linux" => Ok(Architecture::Aarch64Linux),
            "x86_64-darwin" => Ok(Architecture::X86_64Darwin),
            "aarch64-darwin" => Ok(Architecture::Aarch64Darwin),
            _ => Err(format!("Unknown architecture: {}", s)),
        }
    }
}

impl std::convert::TryFrom<&str> for Architecture {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        match s {
            "x86_64-linux" => Ok(Architecture::X86_64Linux),
            "aarch64-linux" => Ok(Architecture::Aarch64Linux),
            "x86_64-darwin" => Ok(Architecture::X86_64Darwin),
            "aarch64-darwin" => Ok(Architecture::Aarch64Darwin),
            _ => Err(format!("Unknown architecture: {}", s)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "server")]
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
}

impl ActiveModelBehavior for ActiveModel {}
