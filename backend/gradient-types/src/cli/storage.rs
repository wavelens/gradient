/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct StorageArgs {
    #[arg(long, env = "GRADIENT_STORE_PATH")]
    pub store_path: Option<String>,
    #[arg(long, env = "GRADIENT_BASE_PATH", default_value = ".")]
    pub base_path: String,
    #[arg(long, env = "GRADIENT_STATE_FILE")]
    pub state_file: Option<String>,
    /// Validate `--state-file` (schema + cross-references, no database access)
    /// and exit: zero when valid, non-zero on the first batch of errors.
    /// Intended for build-time / CI checks; see the NixOS `validateState`
    /// option. Deliberately has no env var so it never trips a live server.
    #[arg(long)]
    pub validate_state: bool,
    #[arg(long, env = "GRADIENT_DELETE_STATE", default_value = "true")]
    pub delete_state: bool,
    #[arg(long, env = "GRADIENT_KEEP_EVALUATIONS", default_value = "30")]
    pub keep_evaluations: usize,
    /// TTL in hours for cached NAR files that have not been fetched recently.
    /// When expired the NAR is removed from storage and its GC root is deleted.
    /// Defaults to 336 (2 weeks). Set to 0 to disable.
    #[arg(long, env = "GRADIENT_NAR_TTL_HOURS", default_value_t = 336)]
    pub nar_ttl_hours: u64,
    /// Grace period in hours before the GC pass deletes a `derivation` row
    /// that no longer has any referencing `build` rows. The grace lets rapid
    /// re-evaluations reuse a freshly-orphaned derivation without
    /// re-inserting it. Set to 0 to GC immediately.
    #[arg(
        long,
        env = "GRADIENT_KEEP_ORPHAN_DERIVATIONS_HOURS",
        default_value_t = 24
    )]
    pub keep_orphan_derivations_hours: i64,
    /// Grace period in hours before the orphan-files pass reclaims a NAR object
    /// no database row references. Covers the upload window where a NAR is on
    /// disk before its `derivation`/`cached_path` rows commit. Set to 0 to
    /// reclaim immediately (tests only).
    #[arg(long, env = "GRADIENT_NAR_UPLOAD_GRACE_HOURS", default_value_t = 24)]
    pub nar_upload_grace_hours: i64,
    /// Hours after which an "active" evaluation that has not been touched is
    /// presumed wedged and stops blocking the per-project evaluation GC (the
    /// wedged evaluation itself is never deleted). 0 = a wedged evaluation
    /// blocks GC forever.
    #[arg(long, env = "GRADIENT_GC_WEDGED_EVAL_HOURS", default_value_t = 24)]
    pub gc_wedged_eval_hours: i64,
    /// Target uncompressed size in bytes for each zstd log chunk written when a
    /// build finalizes. Chunks split on line boundaries, so an over-long line
    /// may exceed this. Defaults to 262144 (256 KiB).
    #[arg(long, env = "GRADIENT_LOG_CHUNK_BYTES", default_value_t = 262144)]
    pub log_chunk_bytes: usize,
    /// Instance-wide cap on total cached NAR bytes, in gigabytes. When the
    /// stored compressed-NAR total leaves every writable cache for an org with
    /// less than 10 MiB of headroom, new evaluations park in `Waiting`. `0`
    /// (default) disables the instance-wide limit; per-cache limits still apply.
    #[arg(long, env = "GRADIENT_MAX_STORAGE_GB", default_value_t = 0)]
    pub max_storage_gb: i32,
    /// Total byte cap for the fleet-shared eval-cache blobs. The periodic
    /// eviction sweep drops the oldest-`updated_at` rows until the surviving
    /// total is at or under this. Defaults to 10 GiB.
    #[arg(
        long,
        env = "GRADIENT_EVAL_CACHE_MAX_TOTAL_BYTES",
        default_value_t = 10 * 1024 * 1024 * 1024
    )]
    pub eval_cache_max_total_bytes: u64,
    /// Max age in days for an eval-cache blob; older blobs are evicted by the
    /// sweep regardless of the size cap. Defaults to 30.
    #[arg(long, env = "GRADIENT_EVAL_CACHE_MAX_AGE_DAYS", default_value_t = 30)]
    pub eval_cache_max_age_days: u64,
    /// Interval in seconds between eval-cache eviction sweeps. Defaults to 3600.
    #[arg(
        long,
        env = "GRADIENT_EVAL_CACHE_SWEEP_INTERVAL_SECS",
        default_value_t = 3600
    )]
    pub eval_cache_sweep_interval_secs: u64,
    /// Interval in seconds between cache maintenance GC passes. Defaults to 3600.
    #[arg(
        long,
        env = "GRADIENT_CACHE_MAINTENANCE_INTERVAL_SECS",
        default_value_t = 3600
    )]
    pub cache_maintenance_interval_secs: u64,
    /// Interval in seconds between NAR signature backfill sweeps. A freshly
    /// uploaded NAR is signed in place by the upload handler, so this tick is
    /// only a fallback for subscription placeholders and any row left unsigned.
    /// Defaults to 3600.
    #[arg(long, env = "GRADIENT_SIGN_SWEEP_INTERVAL_SECS", default_value_t = 3600)]
    pub sign_sweep_interval_secs: u64,
}

impl Default for StorageArgs {
    fn default() -> Self {
        Self {
            store_path: None,
            base_path: ".".into(),
            state_file: None,
            validate_state: false,
            delete_state: true,
            keep_evaluations: 30,
            nar_ttl_hours: 336,
            keep_orphan_derivations_hours: 24,
            nar_upload_grace_hours: 24,
            gc_wedged_eval_hours: 24,
            log_chunk_bytes: 262144,
            max_storage_gb: 0,
            eval_cache_max_total_bytes: 10 * 1024 * 1024 * 1024,
            eval_cache_max_age_days: 30,
            eval_cache_sweep_interval_secs: 3600,
            cache_maintenance_interval_secs: 3600,
            sign_sweep_interval_secs: 60,
        }
    }
}
