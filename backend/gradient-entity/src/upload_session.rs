/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, UploadSessionId};

/// Build-request upload session. `manifest` is a JSONB array of
/// `{path, hash, size}` objects describing the full repo snapshot;
/// `missing` is a JSONB array of BLAKE3 hex strings the client still
/// owes the server before `dispatch` can proceed.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "upload_session")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: UploadSessionId,
    pub project: ProjectId,
    pub manifest: Json,
    pub missing: Json,
    pub total_size: i64,
    pub created_at: NaiveDateTime,
    pub expires_at: NaiveDateTime,
    pub dispatched_at: Option<NaiveDateTime>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::project::Entity",
        from = "Column::Project",
        to = "super::project::Column::Id",
        on_delete = "Cascade"
    )]
    Project,
}

impl Related<super::project::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Project.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
