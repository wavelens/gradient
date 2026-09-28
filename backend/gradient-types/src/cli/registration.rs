/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct RegistrationArgs {
    #[arg(
        id = "registration-enable",
        long = "registration-enable",
        env = "GRADIENT_REGISTRATION_ENABLE",
        default_value = "true"
    )]
    pub enable: bool,
}

impl Default for RegistrationArgs {
    fn default() -> Self {
        Self { enable: true }
    }
}
