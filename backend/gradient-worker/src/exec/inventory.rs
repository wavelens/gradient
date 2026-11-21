/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use gradient_vm::DeviceInventory;
use gradient_wire::host_resource_project_access::WorkerResourceProjectAccess;
use gradient_wire::types::{
    DeviceClass, DeviceRequest, ExecInventory, GuestCapacity, WarmGuests,
    WorkerExecInventoryResourceProjectAccess,
};

use crate::config::ExecArgs;

pub fn advertised(
    devices: &DeviceInventory,
    config: &ExecArgs,
    worker_id: &str,
    warm_guest_sizes: Vec<WarmGuests>,
    secrets: BTreeMap<String, WorkerResourceProjectAccess>,
    guest_capacity: GuestCapacity,
) -> ExecInventory {
    let project_release_by_resource_name =
        build_exec_inventory_resource_project_access(config, &secrets);
    let mut labels = config.labels.clone();
    if !labels.iter().any(|label| label == worker_id) {
        labels.push(worker_id.to_owned());
    }
    ExecInventory {
        labels,
        devices: devices
            .counts()
            .into_iter()
            .map(|count| DeviceClass {
                name: count.name,
                total: count.total,
                free: count.free,
            })
            .collect(),
        mounts: config
            .mounts
            .iter()
            .map(|mount| mount.name.clone())
            .collect(),
        secrets: secrets.into_keys().collect(),
        allow: config.allow(),
        defaults: config.defaults(),
        warm_guests: warm_guest_sizes.iter().map(|warm| warm.count).sum(),
        guest_capacity,
        warm_guest_size: warm_guest_sizes
            .iter()
            .max_by_key(|warm| warm.count)
            .map(|warm| warm.size)
            .unwrap_or_default(),
        warm_guest_sizes,
        max_server_requested_warm_guests: Some(config.max_pre_warm.saturating_sub(config.pre_warm)),
        project_release_by_resource_name,
    }
}

fn build_exec_inventory_resource_project_access(
    config: &ExecArgs,
    secrets: &BTreeMap<String, WorkerResourceProjectAccess>,
) -> WorkerExecInventoryResourceProjectAccess {
    let mut project_access_by_device_class: BTreeMap<String, WorkerResourceProjectAccess> =
        BTreeMap::new();
    for worker_device_config in &config.devices {
        project_access_by_device_class
            .entry(worker_device_config.name.clone())
            .and_modify(|accumulated_access| {
                *accumulated_access =
                    accumulated_access.intersect_released_projects(&worker_device_config.projects);
            })
            .or_insert_with(|| worker_device_config.projects.clone());
    }
    WorkerExecInventoryResourceProjectAccess {
        released_projects_by_worker_sops_secret_name: secrets.clone(),
        project_access_by_worker_mount: config
            .mounts
            .iter()
            .map(|mount| (mount.name.clone(), mount.projects.clone()))
            .collect(),
        released_projects_by_device_class: project_access_by_device_class,
    }
}

pub fn device_requests(requests: &[DeviceRequest]) -> Vec<(String, u32)> {
    requests
        .iter()
        .map(|request| (request.name.clone(), request.count))
        .collect()
}

#[cfg(test)]
mod tests {
    use gradient_vm::SysfsProbe;
    use gradient_wire::types::GuestSize;

    use super::*;

    #[test]
    fn the_worker_id_is_a_label_and_the_warm_guests_add_up() {
        let devices = DeviceInventory::from_config(&[], &SysfsProbe);
        let config = ExecArgs {
            labels: vec!["gpu".into()],
            ..ExecArgs::default()
        };
        let large = GuestSize {
            cpus: 16,
            memory_mb: 65_536,
            disk_mb: 0,
        };
        let sizes = vec![
            WarmGuests {
                size: GuestSize::default_request().warm(),
                count: 2,
            },
            WarmGuests {
                size: large,
                count: 1,
            },
        ];
        let inventory = advertised(
            &devices,
            &config,
            "worker-7",
            sizes.clone(),
            BTreeMap::new(),
            GuestCapacity::default(),
        );
        assert_eq!(inventory.labels, ["gpu", "worker-7"]);
        assert_eq!(
            (inventory.warm_guests, inventory.warm_guest_size),
            (3, GuestSize::default_request().warm()),
            "a server before version 54 sees the most common warm size"
        );
        assert_eq!(inventory.warm_guest_sizes, sizes);
    }
}
