/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use anyhow::{Context, Result, bail};
use gradient_wire::guest::GuestNetwork;
use ipnet::Ipv4Net;

use crate::guest::guest_mac;

type LeasedSet = Arc<Mutex<BTreeSet<Ipv4Addr>>>;

#[derive(Clone)]
pub struct GuestAddresses {
    network: Ipv4Net,
    nameservers: Vec<String>,
    leased: LeasedSet,
}

impl GuestAddresses {
    pub fn new(subnet: &str, nameservers: &[String]) -> Result<Self> {
        let network: Ipv4Net = subnet.parse().with_context(|| format!("subnet {subnet}"))?;
        if !(8..=30).contains(&network.prefix_len()) {
            bail!("subnet {subnet} needs a prefix length between 8 and 30");
        }
        Ok(Self {
            network: network.trunc(),
            nameservers: nameservers.to_vec(),
            leased: Arc::default(),
        })
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

    pub fn network(&self, cid: u32) -> GuestNetwork {
        let gateway = self.addresses.gateway().to_string();
        let nameservers = match self.addresses.nameservers.as_slice() {
            [] => vec![gateway.clone()],
            configured => configured.to_vec(),
        };
        GuestNetwork {
            address: self.address(),
            prefix: self.addresses.network.prefix_len(),
            gateway,
            nameservers,
            mac: guest_mac(cid),
        }
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
    fn leases_take_the_lowest_free_host_after_the_gateway() {
        let addresses = GuestAddresses::new("10.233.0.0/30", &[]).unwrap();
        let first = addresses.lease().unwrap();
        assert_eq!(
            first.network(3),
            GuestNetwork {
                address: "10.233.0.2".into(),
                prefix: 30,
                gateway: "10.233.0.1".into(),
                nameservers: vec!["10.233.0.1".into()],
                mac: "02:47:52:00:00:03".into(),
            }
        );
        assert!(addresses.lease().is_none(), "a /30 holds one guest");
        drop(first);
        assert_eq!(addresses.lease().unwrap().network(3).address, "10.233.0.2");
    }

    #[test]
    fn an_adopted_address_is_not_leased_again() {
        let addresses = GuestAddresses::new("10.233.0.0/16", &[]).unwrap();
        let _kept = addresses.adopt("10.233.0.2").unwrap();
        assert_eq!(addresses.lease().unwrap().network(3).address, "10.233.0.3");
        assert!(addresses.adopt("10.233.0.2").is_none());
        assert!(addresses.adopt("10.233.0.1").is_none(), "the gateway");
        assert!(addresses.adopt("10.234.0.2").is_none(), "another subnet");
    }

    #[test]
    fn configured_nameservers_replace_the_gateway_as_resolver() {
        let nameservers = ["9.9.9.9".to_owned()];
        let addresses = GuestAddresses::new("10.233.0.0/16", &nameservers).unwrap();
        assert_eq!(
            addresses.lease().unwrap().network(3).nameservers,
            nameservers
        );
    }
}
