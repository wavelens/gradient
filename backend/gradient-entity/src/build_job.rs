/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{BuildJobId, DerivationBuildId, DerivationId, EvaluationId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_job")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: BuildJobId,
    pub evaluation: EvaluationId,
    pub derivation: DerivationId,
    pub derivation_build: DerivationBuildId,
    pub score: f64,
    pub score_breakdown: Json,
    pub aborted: bool,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Evaluation",
        to = "super::evaluation::Column::Id",
        on_delete = "Cascade"
    )]
    Evaluation,
    #[sea_orm(
        belongs_to = "super::derivation_build::Entity",
        from = "Column::DerivationBuild",
        to = "super::derivation_build::Column::Id",
        on_delete = "Cascade"
    )]
    DerivationBuild,
    #[sea_orm(
        belongs_to = "super::derivation::Entity",
        from = "Column::Derivation",
        to = "super::derivation::Column::Id"
    )]
    Derivation,
}

impl ActiveModelBehavior for ActiveModel {}
