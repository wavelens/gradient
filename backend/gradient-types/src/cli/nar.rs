/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::{NonZeroU64, NonZeroUsize};

use clap::Args;
use gradient_derive::config_group;

#[config_group("nar")]
#[derive(Args, Debug, Clone)]
pub struct NarArgs {
    /// Maximum size in bytes of a NAR uploaded to the cache upload endpoint.
    #[arg(default_value = "536870912")]
    pub max_upload_size: NonZeroUsize,

    /// Size in bytes up to which NAR downloads come from the server itself instead of a
    /// presigned S3 URL. NARs up to this size also stay in the in-memory cache. Uploads do not
    /// depend on this value.
    #[arg(default_value_t = 1024 * 1024)]
    pub small_bytes: u64,

    /// Capacity in bytes of the in-memory NAR cache. Set it to `0` to disable the cache.
    #[arg(default_value_t = 512 * 1024 * 1024)]
    pub hot_cache_bytes: u64,

    /// Whether to download NARs committed through presigned S3 uploads and verify their hash,
    /// catching same-length corruption at the cost of a full object read. Size checks run
    /// either way. Passthrough and REST uploads always go through verification.
    #[arg(default_value_t = false)]
    pub verify_digest: bool,

    /// Seconds to wait for a NAR object stream from storage (for example an S3 GET) before
    /// answering the worker with `NarAbort`. Workers retry after a `NarAbort`.
    #[arg(default_value = "60")]
    pub storage_open_timeout_secs: NonZeroU64,

    /// Seconds an outbound `NarPush` chunk can wait for the WebSocket to drain before an abort
    /// of the transfer with `NarAbort`.
    #[arg(default_value = "30")]
    pub send_chunk_timeout_secs: NonZeroU64,

    /// Size in bytes of each outbound `NarPush` chunk, from 4 KiB to 4 MiB.
    /// Smaller chunks can help slow or high-latency links like Tailscale over DERP.
    #[arg(
        id = "nar-chunk-bytes",
        value_parser = clap::value_parser!(u64).range(crate::NAR_CHUNK_BYTES_RANGE),
        default_value_t = 512 * 1024,
    )]
    pub chunk_bytes: u64,

    /// NAR serving tasks that can run at once per worker connection, bounding memory and
    /// storage fan-out for large batches.
    #[arg(default_value = "8")]
    pub max_concurrent_serves: NonZeroUsize,

    /// NAR downloads from storage that can run at once across all connections, keeping the
    /// storage near its best total throughput.
    #[arg(default_value = "16")]
    pub max_concurrent_downloads: NonZeroUsize,

    /// Maximum age in seconds of an unfinished upload staged under
    /// {file}`<services.gradient.baseDir>`, counted from its last write. Older uploads disappear
    /// in the next deep GC. Set it to `0` to keep every unfinished upload.
    #[arg(default_value_t = 86400)]
    pub partial_ttl_secs: u64,
}
