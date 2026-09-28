/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct GcArgs {
    /// Interval in seconds between cache maintenance GC passes. Defaults to 3600.
    #[arg(
        long = "gc-interval-secs",
        env = "GRADIENT_GC_INTERVAL_SECS",
        default_value_t = 3600
    )]
    pub interval_secs: u64,

    /// Hours a cached path outside the live closure (the NAR closure of every
    /// retained evaluation's outputs and `.drv` files) is kept after its last
    /// fetch, or its commit if never fetched. `0` keeps nothing beyond
    /// `nar_upload_grace_hours`, which always applies so a closure member is
    /// never evicted between its own commit and its referrer's.
    #[arg(
        long = "gc-nar-ttl-hours",
        env = "GRADIENT_GC_NAR_TTL_HOURS",
        default_value_t = 336
    )]
    pub nar_ttl_hours: u64,

    /// Grace period in hours before the orphan-files pass reclaims a NAR object
    /// no database row references. Covers the upload window where a NAR is on
    /// disk before its `derivation`/`cached_path` rows commit. Set to 0 to
    /// reclaim immediately (tests only).
    #[arg(
        long = "gc-nar-upload-grace-hours",
        env = "GRADIENT_GC_NAR_UPLOAD_GRACE_HOURS",
        default_value_t = 24
    )]
    pub nar_upload_grace_hours: i64,

    /// Grace period in hours before the GC pass deletes a `derivation` row
    /// that no longer has any referencing `build` rows. The grace lets rapid
    /// re-evaluations reuse a freshly-orphaned derivation without
    /// re-inserting it. Set to 0 to GC immediately.
    #[arg(
        long = "gc-orphan-derivation-hours",
        env = "GRADIENT_GC_ORPHAN_DERIVATION_HOURS",
        default_value_t = 24
    )]
    pub orphan_derivation_hours: i64,

    /// Hours after which an "active" evaluation that has not been touched is
    /// presumed wedged and stops blocking the per-task evaluation GC (the
    /// wedged evaluation itself is never deleted). 0 = a wedged evaluation
    /// blocks GC forever.
    #[arg(
        long = "gc-wedged-eval-hours",
        env = "GRADIENT_GC_WEDGED_EVAL_HOURS",
        default_value_t = 24
    )]
    pub wedged_eval_hours: i64,
}

impl Default for GcArgs {
    fn default() -> Self {
        Self {
            interval_secs: 3600,
            nar_ttl_hours: 336,
            nar_upload_grace_hours: 24,
            orphan_derivation_hours: 24,
            wedged_eval_hours: 24,
        }
    }
}
