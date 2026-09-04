/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result};
use gradient_core::ServerState;
use gradient_graph::Demotion;
use gradient_sources::get_hash_from_path;
use std::sync::Arc;

/// Invalidates a path's cached state across all caches in the graph actor's
/// transaction: the cache link and upstream availability on every matching
/// output, the trusted producers, the `cached_path` rows and the NAR object,
/// the gate flags they backed, and the `cache_derivation` closure assertions of
/// the producers and their transitive dependents.
pub async fn invalidate_cache_for_path(state: Arc<ServerState>, path: String) -> Result<()> {
    let (hash, _package) = get_hash_from_path(path.clone())
        .with_context(|| format!("Failed to parse path {}", path))?;

    state
        .graph
        .demote(Demotion::Path { hash })
        .await
        .context("Failed to invalidate path")?;

    Ok(())
}
