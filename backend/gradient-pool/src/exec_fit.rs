/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use gradient_types::waiting_reason::ExecUnmet;
use gradient_wire::types::{
    DeviceRequest, ExecInventory, ExecRequirement, GuestCapacity, GuestSize, JobKind,
    ResourceAmount,
};

use crate::worker_caps::{DeviceCount, WorkerCaps};

pub(crate) struct ExecFit<'a> {
    worker: &'a WorkerCaps,
    inventory: &'a ExecInventory,
    count: DeviceCount,
}

impl<'a> ExecFit<'a> {
    pub(crate) fn of(worker: &'a WorkerCaps, count: DeviceCount) -> Option<Self> {
        let inventory = worker.exec.as_ref()?;
        worker.takes_jobs_of(&JobKind::Exec).then_some(Self {
            worker,
            inventory,
            count,
        })
    }

    pub(crate) fn fits(&self, req: &ExecRequirement) -> bool {
        self.architecture(&req.architecture)
            && req.labels.iter().all(|label| self.label(label))
            && wanted_devices(req).iter().all(|device| self.device(device))
            && self.guest(&req.guest)
            && req.mounts.iter().all(|mount| self.mount(mount))
            && self.network(req.network)
            && self.ssh(req.ssh)
            && req.ports.iter().all(|&port| self.port(port))
            && self.custom_image(req.custom_image)
            && self.nodes(req.nodes)
            && req.secrets.iter().all(|name| self.secret(name))
            && self.server_secrets(&req.server_secrets)
    }

    fn architecture(&self, architecture: &str) -> bool {
        self.worker.runs_architecture(architecture)
    }

    fn label(&self, label: &str) -> bool {
        has(&self.inventory.labels, label)
    }

    fn device(&self, device: &DeviceRequest) -> bool {
        self.inventory.allow.devices.contains(&device.name)
            && self
                .inventory
                .devices
                .iter()
                .any(|class| class.name == device.name && self.count.of(class) >= device.count)
    }

    fn guest(&self, size: &GuestSize) -> bool {
        self.cpus(size) && self.memory(size) && self.disk(size)
    }

    fn cpus(&self, size: &GuestSize) -> bool {
        self.holds(u64::from(size.cpus), |capacity| &capacity.cpus)
    }

    fn memory(&self, size: &GuestSize) -> bool {
        self.holds(size.memory_mb, |capacity| &capacity.memory_mb)
    }

    fn disk(&self, size: &GuestSize) -> bool {
        self.holds(size.disk_mb, |capacity| &capacity.disk_mb)
    }

    fn holds(&self, asked: u64, amount: fn(&GuestCapacity) -> &ResourceAmount) -> bool {
        let capacity = &self.inventory.guest_capacity;
        capacity.is_unset() || asked <= self.count.amount(amount(capacity))
    }

    fn mount(&self, mount: &str) -> bool {
        has(&self.inventory.mounts, mount) && has(&self.inventory.allow.mounts, mount)
    }

    fn network(&self, wanted: bool) -> bool {
        !wanted || self.inventory.allow.network
    }

    fn ssh(&self, wanted: bool) -> bool {
        !wanted || self.inventory.allow.ssh
    }

    fn port(&self, port: u16) -> bool {
        self.inventory.allow.ports.contains(port)
    }

    fn custom_image(&self, wanted: bool) -> bool {
        !wanted || self.inventory.allow.custom_images
    }

    fn nodes(&self, nodes: u32) -> bool {
        self.inventory.allow.nodes >= nodes.max(1)
    }

    fn secret(&self, name: &str) -> bool {
        has(&self.inventory.secrets, name)
    }

    fn server_secrets(&self, names: &[String]) -> bool {
        names.is_empty()
            || (self.worker.capabilities.secrets
                && !names
                    .iter()
                    .any(|name| self.inventory.secrets.contains(name)))
    }
}

fn has(offered: &[String], name: &str) -> bool {
    offered.iter().any(|offer| offer == name)
}

fn wanted_devices(req: &ExecRequirement) -> Vec<DeviceRequest> {
    let mut wanted: BTreeMap<&str, u32> = BTreeMap::new();
    for device in &req.devices {
        *wanted.entry(device.name.as_str()).or_default() += device.count;
    }
    wanted
        .into_iter()
        .map(|(name, count)| DeviceRequest {
            name: name.to_owned(),
            count,
        })
        .collect()
}

type Candidate<'a> = (ExecFit<'a>, &'a ExecRequirement);

pub fn exec_unmet(architecture: &str, candidates: &[(&WorkerCaps, ExecRequirement)]) -> ExecUnmet {
    let fits: Vec<Candidate<'_>> = candidates
        .iter()
        .filter_map(|(worker, req)| {
            ExecFit::of(worker, DeviceCount::Total)
                .filter(|fit| fit.architecture(architecture))
                .map(|fit| (fit, req))
        })
        .collect();
    let Some((_, first)) = fits.first() else {
        return ExecUnmet {
            architecture: Some(architecture.to_owned()),
            ..ExecUnmet::default()
        };
    };
    let none = |offers: &dyn Fn(&ExecFit<'_>, &ExecRequirement) -> bool| {
        !fits.iter().any(|(fit, req)| offers(fit, req))
    };

    ExecUnmet {
        architecture: None,
        labels: unmet_items(
            &fits,
            |req| req.labels.clone(),
            |fit, label| fit.label(label),
        ),
        devices: unmet_items(&fits, wanted_devices, |fit, device| fit.device(device)),
        mounts: unmet_items(
            &fits,
            |req| req.mounts.clone(),
            |fit, mount| fit.mount(mount),
        ),
        nodes: if none(&|fit, req| fit.nodes(req.nodes)) {
            first.nodes.max(1)
        } else {
            0
        },
        custom_image: none(&|fit, req| fit.custom_image(req.custom_image)),
        secrets: unmet_items(
            &fits,
            |req| req.secrets.clone(),
            |fit, name| fit.secret(name),
        ),
        guest: GuestSize {
            cpus: unmet_amount(first.guest.cpus, none(&|fit, req| fit.cpus(&req.guest))),
            memory_mb: unmet_amount(
                first.guest.memory_mb,
                none(&|fit, req| fit.memory(&req.guest)),
            ),
            disk_mb: unmet_amount(first.guest.disk_mb, none(&|fit, req| fit.disk(&req.guest))),
        },
        network: none(&|fit, req| fit.network(req.network)),
        ssh: none(&|fit, req| fit.ssh(req.ssh)),
        ports: unmet_items(&fits, |req| req.ports.clone(), |fit, &port| fit.port(port)),
    }
}

fn unmet_items<T: PartialEq + Clone>(
    fits: &[Candidate<'_>],
    items: impl Fn(&ExecRequirement) -> Vec<T>,
    offers: impl Fn(&ExecFit<'_>, &T) -> bool,
) -> Vec<T> {
    let mut unmet: Vec<T> = Vec::new();
    for item in fits.iter().flat_map(|(_, req)| items(req)) {
        let offered = fits
            .iter()
            .any(|(fit, req)| items(req).contains(&item) && offers(fit, &item));
        if !offered && !unmet.contains(&item) {
            unmet.push(item);
        }
    }

    unmet
}

fn unmet_amount<T: Default>(asked: T, unmet: bool) -> T {
    if unmet { asked } else { T::default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::{ExecAllow, GradientCapabilities, PortRange, PortRanges};

    fn worker(allow: ExecAllow) -> WorkerCaps {
        WorkerCaps {
            architectures: vec!["x86_64-linux".into()],
            capabilities: GradientCapabilities {
                exec: true,
                ..Default::default()
            },
            exec: Some(ExecInventory {
                allow,
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    fn ports(first: u16, last: u16) -> PortRanges {
        PortRanges(vec![PortRange { first, last }])
    }

    fn requirement(edit: impl FnOnce(&mut ExecRequirement)) -> ExecRequirement {
        let mut req = ExecRequirement {
            architecture: "x86_64-linux".into(),
            nodes: 1,
            ..Default::default()
        };
        edit(&mut req);
        req
    }

    fn unmet_on(workers: &[WorkerCaps], req: &ExecRequirement) -> ExecUnmet {
        let candidates: Vec<(&WorkerCaps, ExecRequirement)> =
            workers.iter().map(|w| (w, req.clone())).collect();
        exec_unmet("x86_64-linux", &candidates)
    }

    #[test]
    fn ssh_no_worker_allows_is_the_only_unmet_field() {
        let w = worker(ExecAllow {
            nodes: 1,
            ..Default::default()
        });
        let req = requirement(|r| r.ssh = true);
        assert!(!w.can_run_exec(&req));
        assert_eq!(
            unmet_on(&[w], &req),
            ExecUnmet {
                ssh: true,
                ..ExecUnmet::default()
            }
        );
    }

    #[test]
    fn network_and_ports_name_only_what_no_worker_allows() {
        let workers = [
            worker(ExecAllow {
                ports: ports(22, 22),
                nodes: 1,
                ..Default::default()
            }),
            worker(ExecAllow {
                ports: ports(8000, 8100),
                nodes: 1,
                ..Default::default()
            }),
        ];
        let req = requirement(|r| {
            r.network = true;
            r.ports = vec![22, 8080, 6006];
        });
        let unmet = unmet_on(&workers, &req);
        assert!(unmet.network);
        assert_eq!(unmet.ports, vec![6006]);
        assert_eq!(unmet.to_string(), "network,port:6006");
    }

    #[test]
    fn a_worker_default_that_turns_on_network_counts_only_for_that_worker() {
        let plain = worker(ExecAllow {
            nodes: 1,
            ..Default::default()
        });
        let with_network = worker(ExecAllow {
            network: true,
            nodes: 1,
            ..Default::default()
        });
        let wants_network = requirement(|r| r.network = true);
        let candidates = [
            (&plain, wants_network.clone()),
            (&with_network, requirement(|_| {})),
        ];
        assert!(exec_unmet("x86_64-linux", &candidates[..1]).network);
        assert!(!exec_unmet("x86_64-linux", &candidates).network);
    }

    #[test]
    fn a_worker_that_fits_leaves_nothing_unmet() {
        let w = worker(ExecAllow {
            network: true,
            ssh: true,
            ports: ports(1024, u16::MAX),
            nodes: 1,
            ..Default::default()
        });
        let req = requirement(|r| {
            r.network = true;
            r.ssh = true;
            r.ports = vec![6006];
        });
        assert!(w.can_run_exec(&req));
        assert_eq!(unmet_on(&[w], &req), ExecUnmet::default());
    }
}
