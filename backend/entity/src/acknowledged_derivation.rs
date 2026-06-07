/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{AcknowledgedDerivationId, DerivationId, UserId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "acknowledged_derivation")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: AcknowledgedDerivationId,
    pub derivation: Option<DerivationId>,
    pub pname: Option<String>,
    pub note: String,
    pub created_by: UserId,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id"
    )]
    Derivation,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    User,
}

impl ActiveModelBehavior for ActiveModel {}
