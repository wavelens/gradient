/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-evaluation, per-derivation scored dispatch unit. One row per derivation
//! an evaluation needs (UNIQUE on `(evaluation, derivation)`), created when the
//! eval stream resolves its derivations. The actual build state is shared
//! globally on the linked `derivation_build` anchor; this row only attributes
//! an eval's interest and carries its dispatch score.

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
