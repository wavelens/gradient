/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct GcArgs {
    /// Interval in seconds between cache maintenance GC passes. The default is 3600.
    #[arg(
        long = "gc-interval-secs",
        env = "GRADIENT_GC_INTERVAL_SECS",
        default_value_t = 3600
    )]
    pub interval_secs: u64,

    /// Hours to keep a cached path outside the live closure of retained evaluations after its last
    /// fetch, or its upload if never fetched. `--gc-nar-upload-grace-hours` is always applying on
    /// top. `0` is keeping nothing beyond that grace.
    #[arg(
        long = "gc-nar-ttl-hours",
        env = "GRADIENT_GC_NAR_TTL_HOURS",
        default_value_t = 336
    )]
    pub nar_ttl_hours: u64,

    /// Grace period in hours before the orphan-files pass is reclaiming a NAR object without a
    /// referencing database row. The grace is covering the upload window where a NAR is on disk
    /// before its `derivation`/`cached_path` rows commit. Set to 0 to reclaim immediately (tests
    /// only).
    #[arg(
        long = "gc-nar-upload-grace-hours",
        env = "GRADIENT_GC_NAR_UPLOAD_GRACE_HOURS",
        default_value_t = 24
    )]
    pub nar_upload_grace_hours: i64,

    /// Grace period in hours before the GC pass is deleting a `derivation` row outside the build
    /// closure of every retained evaluation. Rapid re-evaluations can reuse a freshly orphaned
    /// derivation within the grace without re-inserting it. Set to 0 to GC immediately.
    #[arg(
        long = "gc-orphan-derivation-hours",
        env = "GRADIENT_GC_ORPHAN_DERIVATION_HOURS",
        default_value_t = 24
    )]
    pub orphan_derivation_hours: i64,

    /// Hours after which an untouched "active" evaluation is presumed wedged and is no longer
    /// blocking the per-task evaluation GC. The wedged evaluation itself is never deleted. A value
    /// of 0 is letting a wedged evaluation block GC forever.
    #[arg(
        long = "gc-wedged-eval-hours",
        env = "GRADIENT_GC_WEDGED_EVAL_HOURS",
        default_value_t = 24
    )]
    pub wedged_eval_hours: i64,

    /// Seconds from the end of one background deep garbage collection round to the start of the
    /// next. `0` is running a round only on request.
    #[arg(
        long = "gc-deep-interval-secs",
        env = "GRADIENT_GC_DEEP_INTERVAL_SECS",
        default_value_t = 3600
    )]
    pub deep_interval_secs: u64,

    /// Milliseconds between two units of a storage migration or a background
    /// deep garbage collection round. A requested round is running its units without a pause.
    #[arg(
        long = "gc-deep-pace-ms",
        env = "GRADIENT_GC_DEEP_PACE_MS",
        default_value_t = 1000
    )]
    pub deep_pace_ms: u64,
}

impl Default for GcArgs {
    fn default() -> Self {
        Self {
            interval_secs: 3600,
            nar_ttl_hours: 336,
            nar_upload_grace_hours: 24,
            orphan_derivation_hours: 24,
            wedged_eval_hours: 24,
            deep_interval_secs: 3600,
            deep_pace_ms: 1000,
        }
    }
}
