/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{BuildId, BuildLogChunkId};

#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "build_log_chunk")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: BuildLogChunkId,
    pub build: BuildId,
    pub chunk_index: i32,
    pub byte_start: i64,
    pub byte_len: i32,
    pub line_start: i64,
    pub line_count: i32,
    pub compressed_size: i32,
    pub color_prefix: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
