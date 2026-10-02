/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct CacheArgs {
    /// Maximum simultaneous outbound upstream narinfo requests across the whole server. Eval-time
    /// substitutability probes and worker cache-query probes are sharing this pool. A huge
    /// evaluation is never fanning out one request per derivation times every upstream at once.
    #[arg(
        long = "cache-upstream-query-concurrency",
        env = "GRADIENT_CACHE_UPSTREAM_QUERY_CONCURRENCY",
        default_value_t = 32
    )]
    pub upstream_query_concurrency: usize,

    /// Instance-wide cap on total cached NAR bytes, in gigabytes. New evaluations are parking in
    /// `Waiting` once every writable cache for a project has less than 10 MiB of headroom under it.
    /// `0` (default) is disabling the instance-wide limit. Per-cache limits are still applying.
    #[arg(
        long = "cache-max-storage-gb",
        env = "GRADIENT_CACHE_MAX_STORAGE_GB",
        default_value_t = 0
    )]
    pub max_storage_gb: i32,

    /// Interval in seconds between NAR signature backfill sweeps. The upload handler is signing a
    /// freshly uploaded NAR in place. This tick is only a fallback for subscription placeholders
    /// and any row left unsigned. The default is 3600.
    #[arg(
        long = "cache-sign-sweep-interval-secs",
        env = "GRADIENT_CACHE_SIGN_SWEEP_INTERVAL_SECS",
        default_value_t = 3600
    )]
    pub sign_sweep_interval_secs: u64,

    /// Interval in seconds between DWARF build-id index backfill passes. Uploads are indexing their
    /// own NAR in place. This tick is only catching paths cached before the index existed and walks
    /// lost to a restart. The default is 300.
    #[arg(
        long = "cache-debug-index-interval-secs",
        env = "GRADIENT_CACHE_DEBUG_INDEX_INTERVAL_SECS",
        default_value_t = 300
    )]
    pub debug_index_interval_secs: u64,
}

impl Default for CacheArgs {
    fn default() -> Self {
        super::clap_defaults()
    }
}

#[cfg(test)]
mod tests {
    use super::CacheArgs;
    use clap::Parser;

    #[derive(Parser)]
    struct Flags {
        #[command(flatten)]
        cache: CacheArgs,
    }

    #[test]
    fn the_default_is_what_the_command_line_leaves_unset() {
        let parsed = Flags::try_parse_from(["gradient-server"]).expect("no flag is required");
        assert_eq!(
            format!("{:?}", CacheArgs::default()),
            format!("{:?}", parsed.cache)
        );
    }
}
