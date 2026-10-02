/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct GradientCiArgs {
    #[arg(
        id = "gradient-ci-enable",
        long = "gradient-ci-enable",
        env = "GRADIENT_GRADIENT_CI_ENABLE",
        default_value = "true"
    )]
    pub enable: bool,

    #[arg(
        id = "gradient-ci-url",
        long = "gradient-ci-url",
        env = "GRADIENT_GRADIENT_CI_URL",
        default_value = "https://servers.gradient.ci"
    )]
    pub url: String,
}

impl Default for GradientCiArgs {
    fn default() -> Self {
        super::clap_defaults()
    }
}
