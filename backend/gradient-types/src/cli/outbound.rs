/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use clap::Args;
use gradient_derive::config_group;

#[config_group("outbound")]
#[derive(Args, Debug, Clone)]
pub struct OutboundConnectionArgs {
    /// The server can reach private network targets through URLs that project members enter. These targets include upstream caches, APIs of git hosts, actions and outbound webhooks. Each entry can be a CIDR range (`10.0.0.0/8`), an IP address, or an exact-match hostname (`git.internal.example`). Default settings allow the server to connect only to public addresses. Addresses of the server's own network interfaces don't count as public. They need entries too. The server doesn't detect addresses that network configuration can forward to the host without assigning them to an interface. Examples include kube-proxy service addresses and AnyIP routes. List self-hosted git hosts and internal caches here. Project members who can set such URLs can reach all listed targets. Servers trust the listed hostnames for every address that those hostnames resolve to. Loopback addresses and addresses for cloud metadata such as 169.254.169.254 also receive that trust. List only hostnames within managed DNS zones. The server can always connect to hosts in {option}`services.gradient.giteaApp.instanceBaseUrl`.
    #[arg(value_delimiter = ',')]
    pub allowed_non_public_network_targets: Vec<String>,
}
