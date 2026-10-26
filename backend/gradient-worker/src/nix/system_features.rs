/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::Result;
use nix_bindings::{Context, Store};
use std::sync::Arc;

pub async fn detect() -> Vec<String> {
    let features = tokio::task::spawn_blocking(store_system_features)
        .await
        .map_err(anyhow::Error::from)
        .and_then(|features| features);
    features.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "could not read the store's system features; advertising none");
        Vec::new()
    })
}

#[expect(
    clippy::arc_with_non_send_sync,
    reason = "nix-bindings takes its context as an Arc"
)]
fn store_system_features() -> Result<Vec<String>> {
    let ctx = Arc::new(Context::new()?);
    let store = Store::open(&ctx, None)?;
    Ok(store.system_features()?)
}
