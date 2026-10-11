/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("gradient_ci")]
#[derive(Args, Debug, Clone)]
pub struct GradientCiArgs {
    #[arg(id = "gradient-ci-enable", default_value = "true")]
    pub enable: bool,

    #[arg(id = "gradient-ci-url", default_value = "https://servers.gradient.ci")]
    pub url: String,
}
