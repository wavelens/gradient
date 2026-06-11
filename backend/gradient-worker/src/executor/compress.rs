/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Dump store paths to NAR and compress with zstd before upload.
//!
//! Uses `harmonia-file-nar`'s `NarByteStream` for pure-Rust NAR packing (no `nix nar`
//! subprocess). The compressed data is pushed to the server in 64 KiB chunks via
//! [`ClientMessage::NarPush`] - delegated to [`crate::proto::nar::push_direct`].
//!
//! The worker always compresses before upload; the server never sees or
//! writes an uncompressed NAR.

use anyhow::Result;
use tokio::sync::watch;
use tracing::info;

use crate::executor::check_abort;
use crate::nix::store::LocalNixStore;
use crate::proto::job::JobUpdater;
use crate::proto::nar;

/// Compress every path in `store_paths` into a zstd NAR and push it to the
/// server via direct WebSocket transfer. Used for fetched flake inputs,
/// evaluated `.drv` files, and built outputs.
///
/// `abort` is checked before each path. When the server signals `AbortJob`
/// (e.g. the session NAR upload buffer was exceeded) the loop bails with an
/// error so the outer job resolves to `JobFailed` instead of `JobCompleted`.
pub async fn compress_and_push_paths(
    store: &LocalNixStore,
    store_paths: &[String],
    updater: &mut JobUpdater,
    abort: &mut watch::Receiver<bool>,
) -> Result<()> {
    if store_paths.is_empty() {
        return Ok(());
    }

    updater.report_compressing()?;

    for store_path in store_paths {
        check_abort(abort)?;
        nar::push_direct(
            &updater.job_id.clone(),
            store_path,
            &updater.writer,
            &updater.nar_recv,
            Some(store),
        )
        .await?;
        info!(store_path, "compressed and pushed NAR");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::executor::check_abort;
    use tokio::sync::watch;

    #[test]
    fn check_abort_returns_ok_when_not_aborted() {
        let (_tx, mut rx) = watch::channel(false);
        assert!(check_abort(&mut rx).is_ok());
    }

    #[test]
    fn check_abort_returns_err_after_signal() {
        // Regression: prior to this fix `execute_build_job` ignored the
        // abort watch entirely. With it wired in, `compress_and_push_paths`
        // must surface the abort as an `Err` so the surrounding job
        // resolves to `JobFailed` instead of `JobCompleted`.
        let (tx, mut rx) = watch::channel(false);
        tx.send(true).unwrap();
        let err = check_abort(&mut rx).unwrap_err();
        assert!(err.to_string().contains("aborted"), "got: {err}");
    }
}
