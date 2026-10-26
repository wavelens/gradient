/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ClusterJobId, EvaluationId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "eval_split")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub evaluation: EvaluationId,
    pub cluster_job: ClusterJobId,
    pub source_store_path: Option<String>,
    pub fingerprint: Option<String>,
    pub planned_at: Option<NaiveDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cluster_job::Entity",
        from = "Column::ClusterJob",
        to = "super::cluster_job::Column::Id",
        on_delete = "Cascade"
    )]
    ClusterJob,
}

impl ActiveModelBehavior for ActiveModel {}
