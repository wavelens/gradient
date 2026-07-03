/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Sidecar for an `input_update` evaluation: the requested input set the server
//! recorded at trigger time plus the candidate lock and actual bumps the worker
//! reports back during the Fetching state.

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EvaluationId, EvaluationInputUpdateId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "evaluation_input_update")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: EvaluationInputUpdateId,
    #[sea_orm(unique)]
    pub evaluation: EvaluationId,
    pub base_commit: String,
    /// `PatchGeneratorKind` as snake_case, e.g. `flake_lock`.
    pub generator: String,
    /// Requested input names; an empty array means "all tracked inputs".
    pub target_inputs: Json,
    /// Worker-produced candidate `flake.lock` (utf-8), `None` until reported.
    #[sea_orm(column_type = "Text", nullable)]
    pub candidate_lock: Option<String>,
    /// Worker-reported actual bumps `[{name, old_rev, new_rev}]`.
    pub bumped_inputs: Option<Json>,
    pub created_at: NaiveDateTime,
    pub updated_at: NaiveDateTime,
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
}

impl Related<super::evaluation::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Evaluation.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
