/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::DerivationId;

/// Which of the two dependency relations an edge carries. One row per pair, so an
/// edge that is both a build input and a runtime reference is `Both`.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, EnumIter, DeriveActiveEnum, Deserialize, Serialize,
)]
#[sea_orm(rs_type = "i16", db_type = "SmallInteger")]
pub enum EdgeKind {
    /// From the `.drv`: an input the builder needs.
    #[default]
    #[sea_orm(num_value = 0)]
    Buildtime = 0,
    /// Learned from a narinfo or a NAR: the output references it.
    #[sea_orm(num_value = 1)]
    Runtime = 1,
    #[sea_orm(num_value = 2)]
    Both = 2,
}

impl EdgeKind {
    pub const fn is_buildtime(self) -> bool {
        matches!(self, Self::Buildtime | Self::Both)
    }

    pub const fn is_runtime(self) -> bool {
        matches!(self, Self::Runtime | Self::Both)
    }
}

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "derivation_dependency")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub derivation: DerivationId,
    #[sea_orm(primary_key, auto_increment = false)]
    pub dependency: DerivationId,
    pub kind: EdgeKind,
}

#[derive(Copy, Clone, Debug, EnumIter)]
pub enum Relation {
    Derivation,
    Dependency,
}

impl RelationTrait for Relation {
    fn def(&self) -> RelationDef {
        match self {
            Self::Derivation => Entity::belongs_to(super::derivation::Entity)
                .from(Column::Derivation)
                .to(super::derivation::Column::Id)
                .into(),
            Self::Dependency => Entity::belongs_to(super::derivation::Entity)
                .from(Column::Dependency)
                .to(super::derivation::Column::Id)
                .into(),
        }
    }
}

impl ActiveModelBehavior for ActiveModel {}
