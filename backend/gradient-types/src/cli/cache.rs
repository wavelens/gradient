/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("cache")]
#[derive(Args, Debug, Clone)]
pub struct CacheArgs {
    /// Maximum simultaneous outbound upstream narinfo requests across the whole server. Eval-time
    /// substitutability probes and worker cache-query probes are sharing this pool. A huge
    /// evaluation is never fanning out one request per derivation times every upstream at once.
    #[arg(default_value_t = 32)]
    pub upstream_query_concurrency: usize,

    /// Instance-wide cap on total cached NAR bytes, in gigabytes. New evaluations are parking in
    /// `Waiting` once every writable cache for a project has less than 10 MiB of headroom under it.
    /// `0` (default) is disabling the instance-wide limit. Per-cache limits are still applying.
    #[arg(default_value_t = 0)]
    pub max_storage_gb: i32,

    /// Interval in seconds between NAR signature backfill sweeps. The upload handler is signing a
    /// freshly uploaded NAR in place. This tick is only a fallback for subscription placeholders
    /// and any row left unsigned. The default is 3600.
    #[arg(default_value_t = 3600)]
    pub sign_sweep_interval_secs: u64,

    /// Interval in seconds between DWARF build-id index backfill passes. Uploads are indexing their
    /// own NAR in place. This tick is only catching paths cached before the index existed and walks
    /// lost to a restart. The default is 300.
    #[arg(default_value_t = 300)]
    pub debug_index_interval_secs: u64,
}
