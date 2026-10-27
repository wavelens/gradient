/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("exec")]
#[derive(Args, Debug, Clone)]
pub struct ExecArgs {
    /// Whether to enable exec jobs. When disabled, exec entry points fail with a message
    /// instead of starting a job.
    #[arg(id = "exec-enable", default_value = "true")]
    pub enable: bool,
}
