/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result, bail};
use gradient_wire::guest::{GuestNetwork, GuestNetworkIpv6};
use ipnet::{Ipv4Net, Ipv6Net};

use crate::guest::guest_mac;

type LeasedSet = Arc<Mutex<BTreeSet<Ipv4Addr>>>;

pub fn parse_guest_subnet(subnet: &str) -> Result<Ipv4Net> {
    let network: Ipv4Net = subnet.parse().with_context(|| format!("subnet {subnet}"))?;
    if !(8..=30).contains(&network.prefix_len()) {
        bail!("subnet {subnet} needs a prefix length between 8 and 30");
    }
    Ok(network.trunc())
}

pub fn parse_guest_subnet_v6(subnet: &str, guest_bridge_ipv4_subnet: Ipv4Net) -> Result<Ipv6Net> {
    let network: Ipv6Net = subnet.parse().with_context(|| format!("subnet {subnet}"))?;
    let longest_prefix = 96 + guest_bridge_ipv4_subnet.prefix_len();
    if !(64..=longest_prefix).contains(&network.prefix_len()) {
        bail!("Use a prefix length between 64 and {longest_prefix} for subnet {subnet}.");
    }
    if network != network.trunc() {
        let network = network.trunc();
        bail!("Host bits in subnet {subnet} are non-zero. Write the network address {network}.");
    }
    Ok(network)
}

pub fn derive_ipv6_address_from_ipv4_host_offset(
    guest_bridge_ipv4_subnet: Ipv4Net,
    guest_bridge_ipv6_subnet: Ipv6Net,
    address: Ipv4Addr,
) -> Option<Ipv6Addr> {
    if !guest_bridge_ipv4_subnet.contains(&address) {
        return None;
    }
    let guest_ipv4_host_offset =
        u32::from(address).checked_sub(u32::from(guest_bridge_ipv4_subnet.network()))?;
    u128::from(guest_bridge_ipv6_subnet.network())
        .checked_add(u128::from(guest_ipv4_host_offset))
        .map(Ipv6Addr::from)
        .filter(|ipv6_address| guest_bridge_ipv6_subnet.contains(ipv6_address))
}

#[derive(Clone)]
pub struct GuestAddresses {
    network: Ipv4Net,
    guest_bridge_ipv6_subnet: Option<Ipv6Net>,
    nameservers: Vec<String>,
    leased: LeasedSet,
}

impl GuestAddresses {
    pub fn new(
        network: Ipv4Net,
        guest_bridge_ipv6_subnet: Option<Ipv6Net>,
        nameservers: &[String],
    ) -> Self {
        Self {
            network,
            guest_bridge_ipv6_subnet,
            nameservers: nameservers.to_vec(),
            leased: Arc::default(),
        }
    }

    pub fn lease(&self) -> Option<AddressLease> {
        let mut leased = lock(&self.leased);
        let address = self
            .network
            .hosts()
            .skip(1)
            .find(|address| !leased.contains(address))?;
        leased.insert(address);
        Some(self.lease_of(address))
    }

    pub fn adopt(&self, address: &str) -> Option<AddressLease> {
        let address: Ipv4Addr = address.parse().ok()?;
        let usable = address > self.gateway() && address < self.network.broadcast();
        (usable && lock(&self.leased).insert(address)).then(|| self.lease_of(address))
    }

    fn gateway(&self) -> Ipv4Addr {
        self.network
            .hosts()
            .next()
            .unwrap_or(self.network.network())
    }

    fn lease_of(&self, address: Ipv4Addr) -> AddressLease {
        AddressLease {
            address,
            addresses: self.clone(),
        }
    }
}

pub struct AddressLease {
    address: Ipv4Addr,
    addresses: GuestAddresses,
}

impl AddressLease {
    pub fn address(&self) -> String {
        self.address.to_string()
    }

    pub fn network(&self, cid: u32) -> Result<GuestNetwork> {
        let gateway = self.addresses.gateway().to_string();
        let nameservers = match self.addresses.nameservers.as_slice() {
            [] => vec![gateway.clone()],
            configured => configured.to_vec(),
        };
        Ok(GuestNetwork {
            address: self.address(),
            prefix: self.addresses.network.prefix_len(),
            gateway,
            nameservers,
            mac: guest_mac(cid),
            ipv6_configuration: self.guest_network_ipv6_configuration()?,
        })
    }

    fn guest_network_ipv6_configuration(&self) -> Result<Option<GuestNetworkIpv6>> {
        let guest_bridge_ipv4_subnet = self.addresses.network;
        let Some(guest_bridge_ipv6_subnet) = self.addresses.guest_bridge_ipv6_subnet else {
            return Ok(None);
        };
        let ipv6_address_string_from_ipv4_host_offset = |address| {
            derive_ipv6_address_from_ipv4_host_offset(
                guest_bridge_ipv4_subnet,
                guest_bridge_ipv6_subnet,
                address,
            )
            .map(|ipv6_address| ipv6_address.to_string())
            .with_context(|| {
                format!("There is no corresponding IPv6 address in {guest_bridge_ipv6_subnet} for the guest address {address} in the guest subnet {guest_bridge_ipv4_subnet}. The worker won't start a guest with no configured IPv6 address.")
            })
        };
        Ok(Some(GuestNetworkIpv6 {
            address: ipv6_address_string_from_ipv4_host_offset(self.address)?,
            prefix: guest_bridge_ipv6_subnet.prefix_len(),
            gateway: ipv6_address_string_from_ipv4_host_offset(self.addresses.gateway())?,
        }))
    }
}

impl Drop for AddressLease {
    fn drop(&mut self) {
        lock(&self.addresses.leased).remove(&self.address);
    }
}

fn lock(leased: &LeasedSet) -> MutexGuard<'_, BTreeSet<Ipv4Addr>> {
    leased.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leased_guest_ipv6_address_uses_leased_ipv4_host_offset() {
        let network = parse_guest_subnet("10.233.0.0/16").unwrap();
        let guest_bridge_ipv6_subnet = parse_guest_subnet_v6("fd00:ec2::/64", network).unwrap();
        let addresses = GuestAddresses::new(network, Some(guest_bridge_ipv6_subnet), &[]);
        let _kept = addresses.adopt("10.233.1.7").unwrap();
        let leased_guest_address = addresses.lease().unwrap();
        assert_eq!(leased_guest_address.address(), "10.233.0.2");
        assert_eq!(
            leased_guest_address.network(3).unwrap().ipv6_configuration,
            Some(GuestNetworkIpv6 {
                address: "fd00:ec2::2".into(),
                prefix: 64,
                gateway: "fd00:ec2::1".into(),
            })
        );
        let guest_network_with_adopted_ipv4_address =
            addresses.adopt("10.233.1.8").unwrap().network(3).unwrap();
        assert_eq!(
            guest_network_with_adopted_ipv4_address
                .ipv6_configuration
                .unwrap()
                .address,
            "fd00:ec2::108"
        );
    }

    #[test]
    fn network_returns_error_when_ipv6_guest_subnet_is_set_but_lease_has_no_derived_ipv6_address() {
        let network = parse_guest_subnet("10.233.0.0/16").unwrap();
        let addresses = GuestAddresses::new(network, Some("fd00:ec2::/124".parse().unwrap()), &[]);
        assert!(addresses.lease().unwrap().network(3).is_ok());
        assert!(addresses.adopt("10.233.1.7").unwrap().network(3).is_err());
    }

    #[test]
    fn leased_guest_without_ipv6_subnet_has_no_ipv6_configuration() {
        let network = parse_guest_subnet("10.233.0.0/16").unwrap();
        let addresses = GuestAddresses::new(network, None, &[]);
        assert_eq!(
            addresses
                .lease()
                .unwrap()
                .network(3)
                .unwrap()
                .ipv6_configuration,
            None
        );
    }

    #[test]
    fn test_ipv6_prefix_length_96_plus_ipv4_prefix_length_with_largest_ipv4_host_offset_maps_inside_guest_ipv6_subnet()
     {
        for (
            ipv4_subnet_text,
            ipv6_subnet_text,
            last_usable_ipv4_address_text,
            expected_ipv6_address_text,
        ) in [
            (
                "10.233.0.0/16",
                "fd00:ec2::/112",
                "10.233.255.254",
                "fd00:ec2::fffe",
            ),
            (
                "10.233.0.0/30",
                "fd00:ec2::/126",
                "10.233.0.2",
                "fd00:ec2::2",
            ),
            (
                "10.0.0.0/8",
                "fd00:ec2::/104",
                "10.255.255.254",
                "fd00:ec2::ff:fffe",
            ),
        ] {
            let guest_bridge_ipv4_subnet = parse_guest_subnet(ipv4_subnet_text).unwrap();
            let guest_bridge_ipv6_subnet =
                parse_guest_subnet_v6(ipv6_subnet_text, guest_bridge_ipv4_subnet).unwrap();
            let ipv6_address_for_last_ipv4_host_offset = derive_ipv6_address_from_ipv4_host_offset(
                guest_bridge_ipv4_subnet,
                guest_bridge_ipv6_subnet,
                last_usable_ipv4_address_text.parse().unwrap(),
            )
            .unwrap();
            assert_eq!(
                ipv6_address_for_last_ipv4_host_offset,
                expected_ipv6_address_text.parse::<Ipv6Addr>().unwrap(),
                "{ipv6_subnet_text}"
            );
            assert!(
                guest_bridge_ipv6_subnet.contains(&ipv6_address_for_last_ipv4_host_offset),
                "{ipv6_subnet_text}"
            );
        }
        for (
            ipv4_subnet_text,
            ipv6_subnet_text,
            boundary_ipv4_address_text,
            expected_ipv6_address_text,
        ) in [
            (
                "10.233.0.0/16",
                "fd00:ec2::/112",
                "10.233.0.0",
                "fd00:ec2::",
            ),
            (
                "10.233.0.0/16",
                "fd00:ec2::/112",
                "10.233.0.1",
                "fd00:ec2::1",
            ),
            (
                "10.233.0.0/16",
                "fd00:ec2::/112",
                "10.233.255.255",
                "fd00:ec2::ffff",
            ),
            (
                "10.233.0.0/30",
                "fd00:ec2::/126",
                "10.233.0.0",
                "fd00:ec2::",
            ),
            (
                "10.233.0.0/30",
                "fd00:ec2::/126",
                "10.233.0.1",
                "fd00:ec2::1",
            ),
            (
                "10.233.0.0/30",
                "fd00:ec2::/126",
                "10.233.0.3",
                "fd00:ec2::3",
            ),
        ] {
            let guest_bridge_ipv4_subnet = parse_guest_subnet(ipv4_subnet_text).unwrap();
            let guest_bridge_ipv6_subnet =
                parse_guest_subnet_v6(ipv6_subnet_text, guest_bridge_ipv4_subnet).unwrap();
            let ipv6_address_for_boundary_ipv4_address = derive_ipv6_address_from_ipv4_host_offset(
                guest_bridge_ipv4_subnet,
                guest_bridge_ipv6_subnet,
                boundary_ipv4_address_text.parse().unwrap(),
            )
            .unwrap();
            assert_eq!(
                ipv6_address_for_boundary_ipv4_address,
                expected_ipv6_address_text.parse::<Ipv6Addr>().unwrap(),
                "{boundary_ipv4_address_text}"
            );
            assert!(
                guest_bridge_ipv6_subnet.contains(&ipv6_address_for_boundary_ipv4_address),
                "{boundary_ipv4_address_text}"
            );
        }
    }

    #[test]
    fn derive_guest_ipv6_address_from_ipv4_outside_subnet_returns_none() {
        let guest_bridge_ipv4_subnet = parse_guest_subnet("10.233.0.0/16").unwrap();
        let guest_bridge_ipv6_subnet =
            parse_guest_subnet_v6("fd00:ec2::/112", guest_bridge_ipv4_subnet).unwrap();
        for outside_guest_ipv4_subnet_address_text in [
            "10.232.255.255",
            "0.0.0.0",
            "10.234.0.0",
            "255.255.255.255",
            "192.168.1.5",
        ] {
            assert_eq!(
                derive_ipv6_address_from_ipv4_host_offset(
                    guest_bridge_ipv4_subnet,
                    guest_bridge_ipv6_subnet,
                    outside_guest_ipv4_subnet_address_text.parse().unwrap(),
                ),
                None,
                "{outside_guest_ipv4_subnet_address_text}"
            );
        }
    }

    #[test]
    fn leases_take_the_lowest_free_host_after_the_gateway() {
        let network = parse_guest_subnet("10.233.0.0/30").unwrap();
        let addresses = GuestAddresses::new(network, None, &[]);
        let first = addresses.lease().unwrap();
        assert_eq!(
            first.network(3).unwrap(),
            GuestNetwork {
                address: "10.233.0.2".into(),
                prefix: 30,
                gateway: "10.233.0.1".into(),
                nameservers: vec!["10.233.0.1".into()],
                mac: "02:47:52:00:00:03".into(),
                ipv6_configuration: None,
            }
        );
        assert!(addresses.lease().is_none(), "a /30 holds one guest");
        drop(first);
        assert_eq!(
            addresses.lease().unwrap().network(3).unwrap().address,
            "10.233.0.2"
        );
    }

    #[test]
    fn test_ipv6_guest_subnet_rejects_prefix_shorter_than_64_too_few_host_bits_or_set_host_bits_and_accepts_valid()
     {
        let guest_bridge_ipv4_subnet = parse_guest_subnet("10.233.0.0/16").unwrap();
        for refused in [
            "fd00:ec2::/63",
            "fd00:ec2::/113",
            "fd00:ec2::5/64",
            "10.233.0.0/16",
        ] {
            assert!(
                parse_guest_subnet_v6(refused, guest_bridge_ipv4_subnet).is_err(),
                "{refused}"
            );
        }
        for accepted in ["fd00:ec2::/64", "fd00:ec2::/112"] {
            assert_eq!(
                parse_guest_subnet_v6(accepted, guest_bridge_ipv4_subnet).unwrap(),
                accepted.parse::<Ipv6Net>().unwrap()
            );
        }
        let guest_bridge_ipv4_subnet_with_24_bit_prefix =
            parse_guest_subnet("10.233.0.0/24").unwrap();
        assert!(
            parse_guest_subnet_v6(
                "fd00:ec2::/121",
                guest_bridge_ipv4_subnet_with_24_bit_prefix
            )
            .is_err()
        );
        assert_eq!(
            parse_guest_subnet_v6(
                "fd00:ec2::/120",
                guest_bridge_ipv4_subnet_with_24_bit_prefix
            )
            .unwrap(),
            "fd00:ec2::/120".parse::<Ipv6Net>().unwrap()
        );
    }

    #[test]
    fn an_adopted_address_is_not_leased_again() {
        let network = parse_guest_subnet("10.233.0.0/16").unwrap();
        let addresses = GuestAddresses::new(network, None, &[]);
        let _kept = addresses.adopt("10.233.0.2").unwrap();
        assert_eq!(
            addresses.lease().unwrap().network(3).unwrap().address,
            "10.233.0.3"
        );
        assert!(addresses.adopt("10.233.0.2").is_none());
        assert!(addresses.adopt("10.233.0.1").is_none(), "the gateway");
        assert!(addresses.adopt("10.234.0.2").is_none(), "another subnet");
    }

    #[test]
    fn configured_nameservers_replace_the_gateway_as_resolver() {
        let nameservers = ["9.9.9.9".to_owned()];
        let network = parse_guest_subnet("10.233.0.0/16").unwrap();
        let addresses = GuestAddresses::new(network, None, &nameservers);
        assert_eq!(
            addresses.lease().unwrap().network(3).unwrap().nameservers,
            nameservers
        );
    }
}
