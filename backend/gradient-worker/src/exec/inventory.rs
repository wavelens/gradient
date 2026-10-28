/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_vm::DeviceInventory;
use gradient_wire::types::{DeviceClass, DeviceRequest, ExecInventory, GuestCapacity};

use crate::config::ExecArgs;

pub fn advertised(
    devices: &DeviceInventory,
    config: &ExecArgs,
    worker_id: &str,
    warm_guests: u32,
    secrets: Vec<String>,
) -> ExecInventory {
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
        secrets,
        allow: config.allow(),
        defaults: config.defaults(),
        warm_guests,
        guest_capacity: GuestCapacity::default(),
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

    use super::*;

    #[test]
    fn the_worker_id_is_a_label() {
        let devices = DeviceInventory::from_config(&[], &SysfsProbe);
        let config = ExecArgs {
            labels: vec!["gpu".into()],
            ..ExecArgs::default()
        };
        let inventory = advertised(&devices, &config, "worker-7", 1, Vec::new());
        assert_eq!(inventory.labels, ["gpu", "worker-7"]);
        assert_eq!(inventory.warm_guests, 1);
    }
}
