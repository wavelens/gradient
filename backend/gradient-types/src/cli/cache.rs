/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct CacheArgs {
    /// Maximum simultaneous outbound upstream narinfo requests across the whole
    /// server (eval-time substitutability probes and worker cache-query probes
    /// share this pool), so a huge evaluation never fans out one request per
    /// derivation times every upstream at once.
    #[arg(
        long = "cache-upstream-query-concurrency",
        env = "GRADIENT_CACHE_UPSTREAM_QUERY_CONCURRENCY",
        default_value_t = 32
    )]
    pub upstream_query_concurrency: usize,

    /// Instance-wide cap on total cached NAR bytes, in gigabytes. When the
    /// stored compressed-NAR total leaves every writable cache for a project with
    /// less than 10 MiB of headroom, new evaluations park in `Waiting`. `0`
    /// (default) disables the instance-wide limit; per-cache limits still apply.
    #[arg(
        long = "cache-max-storage-gb",
        env = "GRADIENT_CACHE_MAX_STORAGE_GB",
        default_value_t = 0
    )]
    pub max_storage_gb: i32,

    /// Interval in seconds between NAR signature backfill sweeps. A freshly
    /// uploaded NAR is signed in place by the upload handler, so this tick is
    /// only a fallback for subscription placeholders and any row left unsigned.
    /// Defaults to 3600.
    #[arg(
        long = "cache-sign-sweep-interval-secs",
        env = "GRADIENT_CACHE_SIGN_SWEEP_INTERVAL_SECS",
        default_value_t = 3600
    )]
    pub sign_sweep_interval_secs: u64,

    /// Interval in seconds between DWARF build-id index backfill passes. Uploads
    /// index their own NAR in place, so this tick only catches paths cached
    /// before the index existed and walks lost to a restart. Defaults to 300.
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
