/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_vm::{DeviceInventory, MountConfig};
use gradient_wire::types::{DeviceClass, DeviceRequest, ExecAllow, ExecDefaults, ExecInventory};

pub fn advertised(
    devices: &DeviceInventory,
    mut labels: Vec<String>,
    worker_id: &str,
    mounts: &[MountConfig],
    allow: ExecAllow,
    defaults: ExecDefaults,
    warm_guests: u32,
) -> ExecInventory {
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
        mounts: mounts.iter().map(|mount| mount.name.clone()).collect(),
        secrets: Vec::new(),
        allow,
        defaults,
        warm_guests,
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
        let inventory = advertised(
            &devices,
            vec!["gpu".into()],
            "worker-7",
            &[],
            ExecAllow::default(),
            ExecDefaults::default(),
            1,
        );
        assert_eq!(inventory.labels, ["gpu", "worker-7"]);
        assert_eq!(inventory.warm_guests, 1);
    }
}
