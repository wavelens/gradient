/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct StateArgs {
    #[arg(long = "state-file", env = "GRADIENT_STATE_FILE")]
    pub file: Option<String>,
    /// Validate `--state-file` and exit, checking schema and cross-references without database
    /// access. The exit code is zero when valid and non-zero on the first batch of errors. It is
    /// intended for build-time and CI checks like the NixOS `state.validate` option. It
    /// deliberately has no env var to never trip a live server.
    #[arg(long = "state-validate")]
    pub validate: bool,
    #[arg(
        long = "state-delete",
        env = "GRADIENT_STATE_DELETE",
        default_value = "true"
    )]
    pub delete: bool,
}

impl Default for StateArgs {
    fn default() -> Self {
        Self {
            file: None,
            validate: false,
            delete: true,
        }
    }
}
