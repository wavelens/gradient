/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Dump store paths to NAR and compress with zstd before upload.
//!
//! Uses `harmonia-file-nar`'s `NarByteStream` for pure-Rust NAR packing (no `nix nar`
//! subprocess). When the cache is S3-backed the worker uploads the compressed
//! NAR straight to object storage via a presigned PUT URL; otherwise it falls
//! back to chunked [`ClientMessage::NarPush`] over the WebSocket.
//!
//! The worker always compresses before upload; the server never sees or
//! writes an uncompressed NAR.

use anyhow::Result;
use tokio::sync::watch;
use tracing::debug;

use crate::executor::check_abort;
use crate::nix::store::LocalNixStore;
use crate::proto::job::JobUpdater;

/// Compress every path in `store_paths` into a zstd NAR and upload it to the
/// cache. The worker first asks the server (`CacheQuery {Push}`) how each path
/// should be uploaded: a presigned S3 PUT straight to object storage when the
/// cache is S3-backed, else a direct WebSocket `NarPush`. Used for built
/// outputs so multi-GB NARs never relay through the server connection.
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

    updater.report_compressing().await?;

    // Push each output's full runtime closure, not just the output itself: the
    // gradient cache must be closure-complete so a downstream build can fetch
    // every reference (a build's input is a dep output *and its closure*).
    // `upload_one_nar` skips members the cache already holds, so this only
    // uploads paths the cache is missing - e.g. a `-source` referenced by a
    // config that would otherwise strand dependents on `InputsUnavailable`.
    let closure: Vec<String> = store
        .collect_runtime_closure(store_paths)
        .await
        .into_iter()
        .collect();

    let entries = super::query_fetched_paths(updater, closure).await;
    for cp in &entries {
        check_abort(abort)?;
        super::upload_one_nar(updater, cp, store).await?;
        debug!(store_path = %cp.path, "compressed and pushed NAR");
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
