/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use tracing::warn;

use crate::config::DeviceConfig;
use crate::state::HeldDevice;

const KVM: &str = "/dev/kvm";

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DeviceUnit {
    Vfio(String),
    Dev(PathBuf),
    Kvm,
    Usb(String),
}

impl DeviceUnit {
    pub fn key(&self) -> String {
        match self {
            Self::Vfio(bdf) => format!("vfio:{bdf}"),
            Self::Dev(path) => format!("dev:{}", path.display()),
            Self::Kvm => "kvm".to_owned(),
            Self::Usb(vendor_product) => format!("usb:{vendor_product}"),
        }
    }
}

pub trait HostProbe {
    fn vfio_bound(&self, bdf: &str) -> bool;
    fn nested_kvm(&self) -> bool;
}

pub struct SysfsProbe;

impl HostProbe for SysfsProbe {
    fn vfio_bound(&self, bdf: &str) -> bool {
        std::fs::read_link(format!("/sys/bus/pci/devices/{bdf}/driver"))
            .ok()
            .and_then(|driver| driver.file_name().map(|name| name == "vfio-pci"))
            .unwrap_or(false)
    }

    fn nested_kvm(&self) -> bool {
        ["kvm_intel", "kvm_amd"].iter().any(|module| {
            std::fs::read_to_string(format!("/sys/module/{module}/parameters/nested"))
                .is_ok_and(|value| matches!(value.trim(), "Y" | "y" | "1"))
        })
    }
}

type HeldSet = Arc<Mutex<BTreeSet<(String, String)>>>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceCount {
    pub name: String,
    pub total: u32,
    pub free: u32,
}

pub struct DeviceInventory {
    classes: BTreeMap<String, Vec<DeviceUnit>>,
    held: HeldSet,
}

impl DeviceInventory {
    pub fn from_config(devices: &[DeviceConfig], probe: &dyn HostProbe) -> DeviceInventory {
        let mut classes: BTreeMap<String, Vec<DeviceUnit>> = BTreeMap::new();
        for device in devices {
            let units = classes.entry(device.name.clone()).or_default();
            units.extend(usable_units(device, probe));
        }
        DeviceInventory {
            classes,
            held: Arc::default(),
        }
    }

    pub fn reserve(&self, requests: &[(String, u32)]) -> Option<DeviceHold> {
        let mut held = lock(&self.held);
        let mut taken: Vec<(String, DeviceUnit)> = Vec::new();
        for (class, count) in requests {
            let units = self.classes.get(class)?;
            let free = units.iter().filter(|unit| {
                let key = (class.clone(), unit.key());
                !held.contains(&key) && !taken.iter().any(|(c, u)| c == class && u == *unit)
            });
            let chosen: Vec<_> = free.take(*count as usize).cloned().collect();
            if chosen.len() < *count as usize {
                return None;
            }
            taken.extend(chosen.into_iter().map(|unit| (class.clone(), unit)));
        }
        for (class, unit) in &taken {
            held.insert((class.clone(), unit.key()));
        }
        Some(DeviceHold {
            units: taken,
            held: Arc::clone(&self.held),
        })
    }

    pub fn adopt(&self, devices: &[HeldDevice]) -> DeviceHold {
        let mut held = lock(&self.held);
        let mut units = Vec::new();
        for device in devices {
            let unit = self
                .classes
                .get(&device.class)
                .and_then(|units| units.iter().find(|unit| unit.key() == device.unit));
            match unit {
                Some(unit) => {
                    held.insert((device.class.clone(), device.unit.clone()));
                    units.push((device.class.clone(), unit.clone()));
                }
                None => warn!(
                    class = %device.class,
                    unit = %device.unit,
                    "held device is no longer configured"
                ),
            }
        }
        DeviceHold {
            units,
            held: Arc::clone(&self.held),
        }
    }

    pub fn counts(&self) -> Vec<DeviceCount> {
        let held = lock(&self.held);
        self.classes
            .iter()
            .map(|(name, units)| {
                let busy = held.iter().filter(|(class, _)| class == name).count();
                DeviceCount {
                    name: name.clone(),
                    total: count(units.len()),
                    free: count(units.len().saturating_sub(busy)),
                }
            })
            .collect()
    }
}

pub struct DeviceHold {
    units: Vec<(String, DeviceUnit)>,
    held: HeldSet,
}

impl DeviceHold {
    pub fn units(&self) -> Vec<(String, DeviceUnit)> {
        self.units.clone()
    }

    pub fn held(&self) -> Vec<HeldDevice> {
        self.units
            .iter()
            .map(|(class, unit)| HeldDevice {
                class: class.clone(),
                unit: unit.key(),
            })
            .collect()
    }
}

impl Drop for DeviceHold {
    fn drop(&mut self) {
        let mut held = lock(&self.held);
        for (class, unit) in &self.units {
            held.remove(&(class.clone(), unit.key()));
        }
    }
}

fn usable_units(device: &DeviceConfig, probe: &dyn HostProbe) -> Vec<DeviceUnit> {
    let mut units = Vec::new();
    for bdf in &device.vfio {
        if probe.vfio_bound(bdf) {
            units.push(DeviceUnit::Vfio(bdf.clone()));
        } else {
            warn!(device = %device.name, %bdf, "skipping a PCI device not bound to vfio-pci");
        }
    }
    if let Some(dev) = &device.dev {
        if dev == Path::new(KVM) {
            if probe.nested_kvm() {
                units.push(DeviceUnit::Kvm);
            } else {
                warn!(device = %device.name, "skipping /dev/kvm without nested KVM on the host");
            }
        } else {
            units.push(DeviceUnit::Dev(dev.clone()));
        }
    }
    if let Some(usb) = &device.usb {
        units.push(DeviceUnit::Usb(usb.clone()));
    }
    units
}

fn lock(held: &HeldSet) -> MutexGuard<'_, BTreeSet<(String, String)>> {
    held.lock().unwrap_or_else(PoisonError::into_inner)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeProbe {
        unbound: Vec<&'static str>,
    }

    impl HostProbe for FakeProbe {
        fn vfio_bound(&self, bdf: &str) -> bool {
            !self.unbound.contains(&bdf)
        }

        fn nested_kvm(&self) -> bool {
            true
        }
    }

    fn gpus(unbound: Vec<&'static str>) -> DeviceInventory {
        let devices = [DeviceConfig {
            name: "gpu:h100".into(),
            vfio: vec!["0000:17:00.0".into(), "0000:18:00.0".into()],
            dev: None,
            usb: None,
        }];
        DeviceInventory::from_config(&devices, &FakeProbe { unbound })
    }

    fn gpu_count(inventory: &DeviceInventory) -> DeviceCount {
        inventory.counts().remove(0)
    }

    #[test]
    fn free_counts_drop_while_a_hold_lives() {
        let inventory = gpus(Vec::new());
        let hold = inventory.reserve(&[("gpu:h100".into(), 1)]).unwrap();
        assert_eq!(
            gpu_count(&inventory),
            DeviceCount {
                name: "gpu:h100".into(),
                total: 2,
                free: 1
            }
        );
        drop(hold);
        assert_eq!(gpu_count(&inventory).free, 2);
    }

    #[test]
    fn a_request_beyond_free_reserves_nothing() {
        let inventory = gpus(Vec::new());
        assert!(inventory.reserve(&[("gpu:h100".into(), 3)]).is_none());
        assert_eq!(gpu_count(&inventory).free, 2);
    }

    #[test]
    fn an_unbound_vfio_device_is_not_advertised() {
        let inventory = gpus(vec!["0000:18:00.0"]);
        assert_eq!(gpu_count(&inventory).total, 1);
    }

    #[test]
    fn devices_of_a_reattached_guest_stay_held() {
        let inventory = gpus(Vec::new());
        let hold = inventory.adopt(&[HeldDevice {
            class: "gpu:h100".into(),
            unit: "vfio:0000:17:00.0".into(),
        }]);
        assert_eq!(gpu_count(&inventory).free, 1);
        assert_eq!(hold.units().len(), 1);
    }
}
