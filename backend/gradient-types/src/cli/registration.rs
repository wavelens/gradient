/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("registration")]
#[derive(Args, Debug, Clone)]
pub struct RegistrationArgs {
    /// Whether to enable self-service user registration.
    #[arg(id = "registration-enable", default_value = "true")]
    pub enable: bool,
}
