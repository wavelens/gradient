/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("gc")]
#[derive(Args, Debug, Clone)]
pub struct GcArgs {
    /// Seconds between garbage collection passes.
    #[arg(default_value_t = 3600)]
    pub interval_secs: u64,

    /// Hours to keep a cached path outside the live closure of retained evaluations after its
    /// last fetch, or its upload if never fetched. These hours always come on top of
    /// {option}`services.gradient.gc.narUploadGraceHours`. Set it to `0` to keep nothing
    /// beyond that grace.
    #[arg(default_value_t = 336)]
    pub nar_ttl_hours: u64,

    /// Hours before the deletion of an unreferenced NAR object, covering the window between its
    /// upload and the commit of its database rows.
    #[arg(default_value_t = 24)]
    pub nar_upload_grace_hours: i64,

    /// Hours before the deletion of a derivation outside the build closure of every retained
    /// evaluation. Quick re-evaluations can reuse the derivation during this grace. Set it to
    /// `0` to delete such derivations on the next run.
    #[arg(default_value_t = 24)]
    pub orphan_derivation_hours: i64,

    /// Hours an evaluation can stay in one phase before counting as stuck. Stuck evaluations no
    /// longer block evaluation garbage collection. Set it to `0` to treat no evaluation as
    /// stuck.
    #[arg(default_value_t = 24)]
    pub wedged_eval_hours: i64,

    /// Seconds from the end of one background deep garbage collection round to the start of the
    /// next. Set it to `0` to run rounds only on request.
    #[arg(default_value_t = 3600)]
    pub deep_interval_secs: u64,

    /// Milliseconds between two units of a storage migration or a background deep garbage
    /// collection round. Requested rounds run their units without a pause.
    #[arg(default_value_t = 1000)]
    pub deep_pace_ms: u64,
}
