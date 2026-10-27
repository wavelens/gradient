/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceConfig {
    pub name: String,
    #[serde(default)]
    pub vfio: Vec<String>,
    pub dev: Option<PathBuf>,
    pub usb: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MountConfig {
    pub name: String,
    pub path: PathBuf,
    #[serde(default, rename = "readOnly")]
    pub read_only: bool,
    #[serde(default, rename = "perJob")]
    pub per_job: bool,
}

#[derive(Clone, Debug)]
pub struct VmConfig {
    pub image: PathBuf,
    pub pre_warm: u32,
    pub max_pre_warm: u32,
    pub devices: Vec<DeviceConfig>,
    pub mounts: Vec<MountConfig>,
    pub bridge: String,
    pub subnet: String,
    pub cloud_hypervisor_bin: String,
    pub virtiofsd_bin: String,
    pub guests_dir: PathBuf,
    pub supervisor_command: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SupervisorConfig {
    pub cloud_hypervisor_bin: String,
    pub virtiofsd_bin: String,
    pub devices: Vec<DeviceConfig>,
    pub mounts: Vec<MountConfig>,
    pub bridge: String,
}

pub fn check_device(device: &DeviceConfig) -> Result<(), String> {
    let sources = [
        !device.vfio.is_empty(),
        device.dev.is_some(),
        device.usb.is_some(),
    ];
    match sources.into_iter().filter(|set| *set).count() {
        1 => Ok(()),
        0 => Err(format!(
            "device {:?} needs one of vfio, dev or usb",
            device.name
        )),
        _ => Err(format!(
            "device {:?} sets more than one of vfio, dev and usb",
            device.name
        )),
    }
}
