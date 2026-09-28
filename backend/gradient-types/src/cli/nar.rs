/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::greater_than_zero;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct NarArgs {
    /// Maximum size in bytes of a NAR upload to `POST /caches/{cache}/nars` (default 512 MiB).
    #[arg(
        long = "nar-max-upload-size",
        env = "GRADIENT_NAR_MAX_UPLOAD_SIZE",
        value_parser = greater_than_zero::<usize>,
        default_value_t = 512 * 1024 * 1024,
    )]
    pub max_upload_size: usize,

    /// A NAR at or under this many bytes is served through the server on download
    /// instead of a presigned S3 URL and kept in the hot cache. Defaults to 1 MiB.
    #[arg(long = "nar-small-bytes", env = "GRADIENT_NAR_SMALL_BYTES", default_value_t = 1024 * 1024)]
    pub small_bytes: u64,

    /// Weighted capacity of the in-memory NAR cache in bytes; 0 disables it.
    /// Defaults to 512 MiB.
    #[arg(
        long = "nar-hot-cache-bytes",
        env = "GRADIENT_NAR_HOT_CACHE_BYTES",
        default_value_t = 512 * 1024 * 1024
    )]
    pub hot_cache_bytes: u64,

    /// When set, the S3 presigned NAR commit path GETs the uploaded object and
    /// recomputes its hash before marking it cached, catching same-length
    /// corruption at the cost of a full object read. Off by default: the presigned
    /// path still HEAD-checks size, and the relayed/REST upload paths always
    /// content-verify since they already hold the bytes in memory.
    #[arg(
        long = "nar-verify-digest",
        env = "GRADIENT_NAR_VERIFY_DIGEST",
        default_value_t = false
    )]
    pub verify_digest: bool,

    /// Maximum time the server will wait to open a NAR object stream
    /// (e.g. S3 GET) before giving up and emitting `NarUnavailable`. A stalled
    /// backend used to silently block the dispatch loop until the worker's
    /// 600 s receive-timeout fired; this caps it.
    #[arg(
        long = "nar-storage-open-timeout-secs",
        env = "GRADIENT_NAR_STORAGE_OPEN_TIMEOUT_SECS",
        default_value_t = 60
    )]
    pub storage_open_timeout_secs: u64,

    /// Maximum time a single outbound `NarPush` chunk may sit in the writer
    /// queue waiting for the WebSocket sink to make progress. Hitting this
    /// timeout indicates a stalled peer / TCP back-pressure and aborts the
    /// transfer with `NarAbort`.
    #[arg(
        long = "nar-send-chunk-timeout-secs",
        env = "GRADIENT_NAR_SEND_CHUNK_TIMEOUT_SECS",
        default_value_t = 30
    )]
    pub send_chunk_timeout_secs: u64,

    /// Maximum number of NAR-serving tasks that may run concurrently per
    /// worker connection. Bounds memory and storage-backend fan-out when a
    /// worker requests many paths in a single batch.
    #[arg(
        long = "nar-max-concurrent-serves",
        env = "GRADIENT_NAR_MAX_CONCURRENT_SERVES",
        default_value_t = 8
    )]
    pub max_concurrent_serves: usize,

    /// TTL in seconds for partially-received relayed uploads (`*.partial`)
    /// under `<base_dir>/nar-partial`. A periodic sweep deletes partials whose
    /// last write is older than this so an abandoned resume can't pin disk
    /// forever. Default 86400 (24 h). Set to 0 to disable the sweep.
    #[arg(
        long = "nar-partial-ttl-secs",
        env = "GRADIENT_NAR_PARTIAL_TTL_SECS",
        default_value_t = 86400
    )]
    pub partial_ttl_secs: u64,
}

impl Default for NarArgs {
    fn default() -> Self {
        Self {
            max_upload_size: 512 * 1024 * 1024,
            small_bytes: 1024 * 1024,
            hot_cache_bytes: 512 * 1024 * 1024,
            verify_digest: false,
            storage_open_timeout_secs: 60,
            send_chunk_timeout_secs: 30,
            max_concurrent_serves: 8,
            partial_ttl_secs: 86400,
        }
    }
}
