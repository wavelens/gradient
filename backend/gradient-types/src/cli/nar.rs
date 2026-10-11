/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::greater_than_zero;
use clap::Args;
use gradient_derive::config_group;

#[config_group("nar")]
#[derive(Args, Debug, Clone)]
pub struct NarArgs {
    /// Maximum size in bytes of a NAR upload to `POST /caches/{cache}/nars` (default 512 MiB).
    #[arg(value_parser = greater_than_zero::<usize>, default_value_t = 512 * 1024 * 1024)]
    pub max_upload_size: usize,

    /// Size in bytes up to which the server is serving a NAR download itself instead of a presigned
    /// S3 URL. NARs up to this size also stay in the in-memory cache. Uploads do not depend on this
    /// value.
    #[arg(default_value_t = 1024 * 1024)]
    pub small_bytes: u64,

    /// Weighted capacity of the in-memory NAR cache in bytes. `0` is disabling it. The default is
    /// 512 MiB.
    #[arg(default_value_t = 512 * 1024 * 1024)]
    pub hot_cache_bytes: u64,

    /// This flag is making the S3 presigned NAR commit path fetch the uploaded object and hash it
    /// again before marking it cached. It is catching same-length corruption at the cost of a full
    /// object read. It is off by default because the presigned path is still checking the size with
    /// HEAD. Granted worker and REST uploads are always verifying content because they already hold
    /// the bytes in memory.
    #[arg(default_value_t = false)]
    pub verify_digest: bool,

    /// Seconds to wait for a NAR object stream from storage (for example an S3 GET) before
    /// answering the worker with `NarAbort`. The worker is retrying after a `NarAbort`.
    #[arg(default_value_t = 60)]
    pub storage_open_timeout_secs: u64,

    /// Maximum time a single outbound `NarPush` chunk may sit in the writer queue waiting for the
    /// WebSocket sink to make progress. Hitting this timeout is indicating a stalled peer or TCP
    /// back-pressure. The transfer is then aborted with `NarAbort`.
    #[arg(default_value_t = 30)]
    pub send_chunk_timeout_secs: u64,

    /// Size in bytes of each outbound `NarPush` chunk, from 4 KiB to 4 MiB. Smaller chunks are
    /// helping slow or high-latency links like Tailscale over DERP.
    #[arg(
        id = "nar-chunk-bytes",
        value_parser = clap::value_parser!(u64).range(crate::NAR_CHUNK_BYTES_RANGE),
        default_value_t = 512 * 1024,
    )]
    pub chunk_bytes: u64,

    /// Maximum number of NAR-serving tasks that may run concurrently per worker connection. The
    /// limit is bounding memory and storage-backend fan-out for a worker requesting many paths in a
    /// single batch.
    #[arg(default_value_t = 8)]
    pub max_concurrent_serves: usize,

    /// NAR downloads from storage that may run at once across all connections, keeping the storage
    /// near its best total throughput.
    #[arg(value_parser = greater_than_zero::<usize>, default_value_t = 16)]
    pub max_concurrent_downloads: usize,

    /// Seconds since the last write of an unfinished upload staged under `<base_dir>`, after which
    /// the next deep GC is removing the upload. `0` is keeping every unfinished upload.
    #[arg(default_value_t = 86400)]
    pub partial_ttl_secs: u64,
}
