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
    /// Validate `--state-file` (schema + cross-references, no database access)
    /// and exit: zero when valid, non-zero on the first batch of errors.
    /// Intended for build-time / CI checks; see the NixOS `state.validate`
    /// option. Deliberately has no env var so it never trips a live server.
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
