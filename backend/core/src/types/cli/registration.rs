/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct RegistrationArgs {
    #[arg(long, env = "GRADIENT_ENABLE_REGISTRATION", default_value = "true")]
    pub enable_registration: bool,
    #[arg(long, env = "GRADIENT_REPORT_ERRORS", default_value = "false")]
    pub report_errors: bool,
}

impl Default for RegistrationArgs {
    fn default() -> Self {
        Self {
            enable_registration: true,
            report_errors: false,
        }
    }
}
