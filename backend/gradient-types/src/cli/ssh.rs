/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::input::port_in_range;
use clap::Args;

#[derive(Args, Debug, Clone)]
pub struct SshArgs {
    #[arg(
        id = "ssh-enable",
        long = "ssh-enable",
        env = "GRADIENT_SSH_ENABLE",
        default_value = "false"
    )]
    pub enable: bool,
    #[arg(long = "ssh-listen-address", env = "GRADIENT_SSH_LISTEN_ADDRESS")]
    pub listen_address: Option<String>,
    #[arg(
        id = "ssh-port",
        long = "ssh-port",
        env = "GRADIENT_SSH_PORT",
        value_parser = port_in_range,
        default_value_t = 2222
    )]
    pub port: u16,
    #[arg(long = "ssh-host-key-file", env = "GRADIENT_SSH_HOST_KEY_FILE")]
    pub host_key_file: Option<String>,
}

impl Default for SshArgs {
    fn default() -> Self {
        Self {
            enable: false,
            listen_address: None,
            port: 2222,
            host_key_file: None,
        }
    }
}
