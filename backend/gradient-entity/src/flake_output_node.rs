/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{EvaluationId, FlakeOutputNodeId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "flake_output_node")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: FlakeOutputNodeId,
    pub evaluation: EvaluationId,
    pub path: String,
    pub parent: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_derivation: bool,
    pub drv_path: Option<String>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::evaluation::Entity",
        from = "Column::Evaluation",
        to = "super::evaluation::Column::Id"
    )]
    Evaluation,
}

impl ActiveModelBehavior for ActiveModel {}

impl From<Model> for gradient_api_types::FlakeGraphNode {
    fn from(n: Model) -> Self {
        Self {
            path: n.path,
            parent: n.parent,
            name: n.name,
            kind: n.kind,
            is_derivation: n.is_derivation,
            drv_path: n.drv_path.map(|p| {
                crate::StorePath::parse_stored(&p)
                    .map(|sp| sp.base())
                    .unwrap_or(p)
            }),
        }
    }
}
