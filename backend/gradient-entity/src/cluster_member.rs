/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A job's cluster reference: exactly one of `evaluation` or `derivation_build`,
//! each unique, so a job belongs to at most one cluster.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ClusterJobId, ClusterMemberId, DerivationBuildId, EvaluationId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cluster_member")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: ClusterMemberId,
    pub cluster_job: ClusterJobId,
    pub evaluation: Option<EvaluationId>,
    pub derivation_build: Option<DerivationBuildId>,
    pub role: String,
    pub primary: bool,
    pub pin: Option<String>,
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
