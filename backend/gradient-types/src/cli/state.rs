/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("state")]
#[derive(Args, Debug, Clone)]
pub struct StateArgs {
    pub file: Option<String>,
    /// Validate `--state-file` and exit, checking schema and cross-references without database
    /// access. The exit code is zero when valid and non-zero on the first batch of errors. It is
    /// intended for build-time and CI checks like the NixOS `state.validate` option. It
    /// deliberately has no env var to never trip a live server.
    #[flag_only]
    pub validate: bool,
    #[arg(default_value = "true")]
    pub delete: bool,
}
