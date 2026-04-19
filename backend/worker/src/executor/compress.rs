/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Compress task — dump store paths to NAR and compress with zstd before upload.
//!
//! Uses `harmonia-nar`'s `NarByteStream` for pure-Rust NAR packing (no `nix nar`
//! subprocess). The compressed data is pushed to the server in 64 KiB chunks via
//! [`ClientMessage::NarPush`] — delegated to [`crate::proto::nar::push_direct`].

use anyhow::Result;
use proto::messages::CompressTask;
use tracing::info;

use crate::nix::store::LocalNixStore;
use crate::proto::job::JobUpdater;
use crate::proto::nar;

/// Compress all store paths in `task` into zstd-compressed NARs and push them
/// to the server via direct WebSocket transfer.
pub async fn compress_outputs(
    _store: &LocalNixStore,
    task: &CompressTask,
    updater: &mut JobUpdater,
) -> Result<()> {
    updater.report_compressing()?;

    for store_path in &task.store_paths {
        nar::push_direct(&updater.job_id.clone(), store_path, &updater.writer, None).await?;
        info!(store_path, "compressed and pushed NAR");
    }

    Ok(())
}
