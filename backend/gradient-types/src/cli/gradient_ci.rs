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
    /// Whether to enable the Gradient.CI Servers offer on the workers pages.
    #[arg(id = "gradient-ci-enable", default_value = "true")]
    pub enable: bool,

    /// Address of Gradient.CI Servers. "Connect" links to `<url>/connect`. New connections go
    /// to the host and port of this address at `/proto`, over `wss://` for `https://` and
    /// `ws://` for `http://`.
    #[arg(id = "gradient-ci-url", default_value = "https://servers.gradient.ci")]
    pub url: String,
}
