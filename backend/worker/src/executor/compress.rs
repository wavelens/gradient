/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Dump store paths to NAR and compress with zstd before upload.
//!
//! Uses `harmonia-nar`'s `NarByteStream` for pure-Rust NAR packing (no `nix nar`
//! subprocess). The compressed data is pushed to the server in 64 KiB chunks via
//! [`ClientMessage::NarPush`] — delegated to [`crate::proto::nar::push_direct`].
//!
//! The worker always compresses before upload; the server never sees or
//! writes an uncompressed NAR.

use anyhow::Result;
use tracing::info;

use crate::nix::store::LocalNixStore;
use crate::proto::job::JobUpdater;
use crate::proto::nar;

/// Compress every path in `store_paths` into a zstd NAR and push it to the
/// server via direct WebSocket transfer. Used for fetched flake inputs,
/// evaluated `.drv` files, and built outputs.
pub async fn compress_and_push_paths(
    store: &LocalNixStore,
    store_paths: &[String],
    updater: &mut JobUpdater,
) -> Result<()> {
    if store_paths.is_empty() {
        return Ok(());
    }

    updater.report_compressing()?;

    for store_path in store_paths {
        nar::push_direct(
            &updater.job_id.clone(),
            store_path,
            &updater.writer,
            Some(store),
        )
        .await?;
        info!(store_path, "compressed and pushed NAR");
    }

    Ok(())
}
