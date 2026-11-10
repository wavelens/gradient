/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::NonZeroU64;

use clap::Args;
use gradient_derive::config_group;

#[config_group("gc")]
#[derive(Args, Debug, Clone)]
pub struct GcArgs {
    /// Seconds between garbage collection passes.
    #[arg(default_value = "3600")]
    pub interval_secs: NonZeroU64,

    /// The number of hours a cache can hold claims for cached paths outside the live closure of
    /// retained evaluations. Start the count at the last fetch of that cache. Start the count at
    /// claim creation when the cache never had a fetch. The system can remove the cached path
    /// itself once the claims on that path drop to zero. Always add this number to
    /// {option}`services.gradient.gc.narUploadGraceHours`. Set this option to `0` to keep nothing
    /// beyond this grace period.
    #[arg(default_value_t = 336)]
    pub nar_ttl_hours: u64,

    /// Hours before deletion of an unreferenced NAR object or build-request blob. These hours cover
    /// the window between upload and the commit of database rows.
    #[arg(default_value_t = 24)]
    pub nar_upload_grace_hours: u32,

    /// Hours before the deletion of a derivation outside the build closure of every retained
    /// evaluation. Quick re-evaluations can reuse the derivation during this grace. Set it to
    /// `0` to delete such derivations on the next run.
    #[arg(default_value_t = 24)]
    pub orphan_derivation_hours: u32,

    /// Seconds from the end of one background deep garbage collection round to the start of the
    /// next. Set it to `0` to run rounds only on request.
    #[arg(default_value_t = 3600)]
    pub deep_interval_secs: u64,

    /// Milliseconds between two units of a storage migration or a background deep garbage
    /// collection round. Requested rounds run their units without a pause.
    #[arg(default_value_t = 1000)]
    pub deep_pace_ms: u64,
}
