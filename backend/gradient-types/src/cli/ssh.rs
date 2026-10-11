/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::port_in_range;
use clap::Args;
use gradient_derive::config_group;

#[config_group("ssh")]
#[derive(Args, Debug, Clone)]
pub struct SshArgs {
    #[arg(id = "ssh-enable", default_value = "false")]
    pub enable: bool,
    /// IP address for the SSH server to listen on. IPv6 addresses also accept IPv4 clients.
    /// The default `::` thus means listening on every address of both families.
    pub listen_address: Option<String>,
    /// Port for the SSH server to listen on.
    #[arg(id = "ssh-port", value_parser = port_in_range, default_value_t = 2222)]
    pub port: u16,
    pub host_key_file: Option<String>,
}
