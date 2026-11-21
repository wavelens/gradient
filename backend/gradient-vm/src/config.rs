/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::PathBuf;

use gradient_wire::host_resource_project_access::WorkerResourceProjectAccess;
use ipnet::{Ipv4Net, Ipv6Net};
use serde::{Deserialize, Serialize};

use crate::state::GuestId;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DeviceConfig {
    pub name: String,
    pub projects: WorkerResourceProjectAccess,
    #[serde(default)]
    pub vfio: Vec<String>,
    pub dev: Option<PathBuf>,
    pub usb: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MountConfig {
    pub name: String,
    pub path: PathBuf,
    pub projects: WorkerResourceProjectAccess,
    #[serde(default, rename = "readOnly")]
    pub read_only: bool,
    #[serde(default, rename = "perJob")]
    pub per_job: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SopsSecretFileEntry {
    pub path: PathBuf,
    pub projects: WorkerResourceProjectAccess,
}

impl MountConfig {
    pub fn host_directory_for_guest(&self, guest_id: &GuestId) -> PathBuf {
        match self.per_job {
            true => self.path.join(guest_id.to_string()),
            false => self.path.clone(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct VmConfig {
    pub image: PathBuf,
    pub pre_warm: u32,
    pub max_pre_warm: u32,
    pub guest_store_mb: u64,
    pub devices: Vec<DeviceConfig>,
    pub mounts: Vec<MountConfig>,
    pub bridge: String,
    pub virtiofsd_bin: String,
    pub guests_dir: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SupervisorConfig {
    pub virtiofsd_bin: String,
    pub devices: Vec<DeviceConfig>,
    pub mounts: Vec<MountConfig>,
    pub bridge: String,
    pub guest_bridge_ipv4_subnet: Option<Ipv4Net>,
    pub guest_bridge_ipv6_subnet: Option<Ipv6Net>,
}

const RESERVED_MOUNT_NAMES: [&str; 3] = ["work", "control", "console"];
const MAX_MOUNT_NAME_LENGTH_BYTES: usize = 64;

pub fn check_mount_name(mount: &MountConfig) -> Result<(), String> {
    let configured_mount_name = &mount.name;
    if configured_mount_name.is_empty()
        || configured_mount_name.len() > MAX_MOUNT_NAME_LENGTH_BYTES
        || !configured_mount_name.bytes().all(|mount_name_byte| {
            mount_name_byte.is_ascii_alphanumeric()
                || mount_name_byte == b'-'
                || mount_name_byte == b'_'
        })
    {
        return Err(format!(
            "mount {name:?} needs a name of 1 to {MAX_MOUNT_NAME_LENGTH_BYTES} bytes of ASCII letters, digits, - or _",
            name = configured_mount_name
        ));
    }
    if RESERVED_MOUNT_NAMES.contains(&configured_mount_name.as_str()) {
        return Err(format!(
            "Reserved name for mount {name:?}. Pick a name other than work, control, console.",
            name = configured_mount_name
        ));
    }
    Ok(())
}

pub fn check_mount_list(mounts: &[MountConfig]) -> Result<(), String> {
    let mut seen_mount_names = std::collections::HashSet::new();
    mounts.iter().try_for_each(|mount| {
        check_mount_name(mount)?;
        if !seen_mount_names.insert(mount.name.as_str()) {
            return Err(format!(
                "duplicate mount name {name:?}. Each mount must have a unique name on the worker.",
                name = mount.name
            ));
        }
        Ok(())
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_name_check_rejects_reserved_slash_and_dotdot_and_accepts_valid_name() {
        let mount_config_with_given_name = |name: &str| MountConfig {
            name: name.to_owned(),
            path: PathBuf::from("/srv/datasets"),
            projects: WorkerResourceProjectAccess::EveryProject,
            read_only: false,
            per_job: false,
        };

        for mount_name_to_reject in ["work", "control", "console", "a/b", "..", "../etc", ""] {
            assert!(
                check_mount_name(&mount_config_with_given_name(mount_name_to_reject)).is_err(),
                "{mount_name_to_reject:?}"
            );
        }
        assert!(check_mount_name(&mount_config_with_given_name(&"a".repeat(65))).is_err());
        assert!(check_mount_name(&mount_config_with_given_name("datasets_2-ro")).is_ok());
    }

    #[test]
    fn mount_or_device_config_without_projects_field_is_rejected() {
        assert!(
            serde_json::from_str::<MountConfig>(r#"{"name":"datasets","path":"/srv/datasets"}"#)
                .is_err()
        );
        assert!(
            serde_json::from_str::<DeviceConfig>(r#"{"name":"kvm","dev":"/dev/kvm"}"#).is_err()
        );
        assert!(
            serde_json::from_str::<MountConfig>(
                r#"{"name":"datasets","path":"/srv/datasets","projects":"all"}"#
            )
            .is_ok()
        );
        assert!(
            serde_json::from_str::<DeviceConfig>(
                r#"{"name":"kvm","dev":"/dev/kvm","projects":[]}"#
            )
            .is_ok()
        );
    }

    fn mount_config_with_name_path_and_project_access(
        name: &str,
        path: &str,
        projects: WorkerResourceProjectAccess,
    ) -> MountConfig {
        MountConfig {
            name: name.to_owned(),
            path: PathBuf::from(path),
            projects,
            read_only: false,
            per_job: false,
        }
    }

    #[test]
    fn mount_list_with_duplicate_mount_names_is_rejected_and_error_names_the_mount() {
        let mounts = [
            mount_config_with_name_path_and_project_access(
                "cache",
                "/srv/a",
                WorkerResourceProjectAccess::OnlyProjects([uuid::Uuid::from_u128(1)].into()),
            ),
            mount_config_with_name_path_and_project_access(
                "cache",
                "/srv/b",
                WorkerResourceProjectAccess::OnlyProjects([uuid::Uuid::from_u128(2)].into()),
            ),
        ];

        let error = check_mount_list(&mounts).unwrap_err();
        assert!(error.contains("\"cache\""), "{error}");
    }

    #[test]
    fn accepts_mounts_with_different_names() {
        let mounts = [
            mount_config_with_name_path_and_project_access(
                "cache",
                "/srv/a",
                WorkerResourceProjectAccess::EveryProject,
            ),
            mount_config_with_name_path_and_project_access(
                "datasets",
                "/srv/b",
                WorkerResourceProjectAccess::EveryProject,
            ),
        ];

        assert_eq!(check_mount_list(&mounts), Ok(()));
    }
}
