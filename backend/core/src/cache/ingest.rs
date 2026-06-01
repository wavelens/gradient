/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::nix_hash::normalize_nar_hash;
use crate::storage::nar::NarStore;
use crate::types::ids::{CacheId, CachedPathId, CachedPathSignatureId, OrganizationId};
use crate::types::*;
use sea_orm::sea_query::OnConflict;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use tracing::warn;

/// NAR metadata required to record a cached path. Hashes are normalized on write.
pub struct IngestInput<'a> {
    pub store_path: &'a str,
    pub file_hash: &'a str,
    pub file_size: i64,
    pub nar_size: i64,
    pub nar_hash: &'a str,
    /// References in hash-name format (no `/nix/store/` prefix).
    pub references: &'a [String],
    pub deriver: Option<&'a str>,
}

/// Which caches get a NULL-signature placeholder so the sign sweep signs later.
pub enum SignTargets {
    /// Every cache owned by this organization (worker build-output path).
    OrgCaches(OrganizationId),
    /// Exactly one cache (direct CLI upload path).
    Cache(CacheId),
}

/// Outcome of an ingest.
pub struct IngestOutcome {
    pub cached_path: CachedPathId,
    /// True when the `cached_path` row was created by this call.
    pub created: bool,
}
