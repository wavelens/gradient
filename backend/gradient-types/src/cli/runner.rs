/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("runner")]
#[derive(Args, Debug, Clone)]
pub struct RunnerArgs {
    /// Whether to enable runner jobs. When disabled, runner entry points fail with a message
    /// instead of starting a job.
    #[arg(id = "runner-enable", default_value = "true")]
    pub enable: bool,
}
