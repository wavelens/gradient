/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use chrono::NaiveDateTime;
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// A cached Nix store path.
///
/// Represents any store path whose NAR is stored in the cache — sources,
/// build outputs, or anything else. The NAR data is stored once (keyed by
/// `hash`). Association with specific caches and their signatures is via
/// `cached_path_signature`.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Deserialize, Serialize)]
#[sea_orm(table_name = "cached_path")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: Uuid,
    /// Full store path, e.g. `/nix/store/abc123-source`.
    pub store_path: String,
    /// The 32-char hash portion of the store path (unique, used for narinfo lookups).
    #[sea_orm(unique)]
    pub hash: String,
    /// Human-readable name portion of the store path.
    pub package: String,
    /// SHA-256 hash of the compressed NAR file (`sha256:<hex>`).
    pub file_hash: Option<String>,
    /// Size in bytes of the compressed NAR file.
    pub file_size: Option<i64>,
    /// Size in bytes of the uncompressed NAR.
    pub nar_size: Option<i64>,
    /// NAR hash in `sha256:<nix32>` format.
    pub nar_hash: Option<String>,
    /// Space-separated list of store-path references (hash-name format).
    pub references: Option<String>,
    /// Content-address field, if the path is content-addressed.
    pub ca: Option<String>,
    pub created_at: NaiveDateTime,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}

impl Model {
    /// Returns `true` when the NAR has been fully uploaded and recorded.
    ///
    /// A `cached_path` row is created eagerly when the path is first seen, but
    /// `file_hash` is only set after the compressed NAR is actually stored. An
    /// absent `file_hash` means the upload is pending or failed.
    pub fn is_fully_cached(&self) -> bool {
        self.file_hash.is_some()
    }
}
