/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{OrganizationId, UploadSessionId};

/// Build-request upload session. `manifest` is a JSONB array of
/// `{path, hash, size}` objects describing the full repo snapshot;
/// `missing` is a JSONB array of BLAKE3 hex strings the client still
/// owes the server before `dispatch` can proceed.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "upload_session")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: UploadSessionId,
    pub organization: OrganizationId,
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
        belongs_to = "super::organization::Entity",
        from = "Column::Organization",
        to = "super::organization::Column::Id",
        on_delete = "Cascade"
    )]
    Organization,
}

impl Related<super::organization::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Organization.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
