/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::num::{NonZeroU64, NonZeroUsize};

use clap::Args;
use gradient_derive::config_group;

#[config_group("upload")]
#[derive(Args, Debug, Clone)]
pub struct UploadArgs {
    /// Number of uploads over 1 MiB (NARs and eval cache segments) admitted at once across all
    /// workers and REST clients. Smaller uploads have a window of 128 of their own. Uploads
    /// hold their permit until the object is in storage. Other uploads wait for a permit in
    /// the meantime.
    #[arg(default_value = "8")]
    pub concurrency: NonZeroUsize,

    /// Total size in bytes of admitted uploads. Uploads that exceed the remaining budget wait.
    /// An upload larger than the whole budget can run alone once nothing else is in flight.
    #[arg(default_value = "8589934592")]
    pub bytes_budget: NonZeroU64,

    /// Seconds granted worker uploads can go without data before they lose their permit and
    /// their workers receive a request to retry.
    #[arg(default_value = "300")]
    pub lease_idle_secs: NonZeroU64,

    /// Seconds NAR uploads to the cache upload endpoint wait for a permit before getting a 503
    /// answer with `Retry-After`.
    #[arg(default_value = "30")]
    pub rest_wait_secs: NonZeroU64,
}
