/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{BuildRequestBlobId, OrganizationId};

/// Content-addressed source-file blob uploaded as part of a build request.
/// The 32-byte BLAKE3 `hash` identifies the payload on the configured
/// storage backend; the row is the GC truth source via `last_used_at`.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_request_blob")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: BuildRequestBlobId,
    pub organization: OrganizationId,
    pub hash: Vec<u8>,
    pub size: i64,
    pub created_at: NaiveDateTime,
    pub last_used_at: NaiveDateTime,
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
