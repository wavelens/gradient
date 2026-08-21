/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

use crate::ids::{CachedPathId, DebugInfoId};

/// One DWARF debug-info file, keyed by its ELF build id.
///
/// Written by the build-id indexer for any cached NAR that carries
/// `lib/debug/.build-id/<xx>/<yy>.debug` members (nixpkgs `separateDebugInfo`
/// outputs). `member` is the path inside the NAR; the serving side pairs it
/// with the `cached_path`'s NAR url to answer `debuginfo/{build_id}` the way
/// nix's own `index-debug-info` binary caches do.
#[derive(Clone, Debug, Default, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "debug_info")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: DebugInfoId,
    /// 40-char lowercase hex ELF build id, without the `.debug` suffix.
    pub build_id: String,
    pub cached_path: CachedPathId,
    /// NAR-relative path of the debug file, e.g. `lib/debug/.build-id/ab/cd.debug`.
    pub member: String,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::cached_path::Entity",
        from = "Column::CachedPath",
        to = "super::cached_path::Column::Id"
    )]
    CachedPath,
}

impl ActiveModelBehavior for ActiveModel {}
