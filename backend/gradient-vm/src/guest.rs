/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, BTreeSet};
use std::net::Ipv4Addr;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use gradient_wire::guest::{
    AGENT_RELAY_PORT_BASE, GuestConnection, GuestNetwork, GuestRelay, GuestRelayKind,
    SupervisorReply, SupervisorRequest,
};
use serde::Serialize;
use tokio::net::UnixStream;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::agent::{GUEST_BOOT_TIMEOUT, GuestBootError, wait_ready};
use crate::config::{MountConfig, VmConfig};
use crate::devices::DeviceUnit;
use crate::hypervisor::{GuestImage, HypervisorApi, vm_config};
use crate::state::{GuestId, GuestState, HeldDevice, StoredRelay, write_state};
use crate::unit::{GuestUnits, unit_name};

pub const WORK_MOUNT_TAG: &str = "work";
const SOCKET_WAIT: Duration = Duration::from_secs(30);
const USBIP_PORT: u16 = 3240;

pub struct PlugPlan {
    pub units: Vec<(String, DeviceUnit)>,
    pub mounts: Vec<(MountConfig, PathBuf)>,
    pub work: PathBuf,
    pub network: Option<GuestNetwork>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PluggedDevices {
    pub guest_devices: BTreeMap<String, Vec<String>>,
    pub relays: Vec<GuestRelay>,
    pub usb_bus_ids: Vec<String>,
    pub groups: Vec<String>,
}

pub struct Guest {
    state: GuestState,
    dir: PathBuf,
    api: HypervisorApi,
    control: Option<GuestConnection<UnixStream>>,
    bridge: String,
}

impl Guest {
    pub fn from_state(config: &VmConfig, state: GuestState) -> Guest {
        let dir = config.guests_dir.join(state.guest_id.to_string());
        Guest {
            api: HypervisorApi::new(state.api_socket.clone()),
            state,
            dir,
            control: None,
            bridge: config.bridge.clone(),
        }
    }

    pub async fn boot(
        units: &GuestUnits,
        config: &VmConfig,
        image: &GuestImage,
        cid: u32,
        stop: &CancellationToken,
    ) -> Result<Guest, GuestBootError> {
        let guest_id = GuestId::new();
        let dir = config.guests_dir.join(guest_id.to_string());
        let state = GuestState {
            unit: unit_name(&guest_id),
            guest_id,
            cid,
            api_socket: dir.join("api.sock"),
            address: None,
            tap: None,
            job: None,
            devices_held: Vec::new(),
            mounts: Vec::new(),
            relays: Vec::new(),
            usb_bus_ids: Vec::new(),
            per_job_dirs: Vec::new(),
        };
        let mut guest = Guest::from_state(config, state);
        let started = match guest.start_unit(units, config).await {
            Ok(()) => tokio::select! {
                started = guest.start_vm(image) => started,
                () = stop.cancelled() => Err(anyhow!("the guest pool stopped during the boot")),
            },
            Err(e) => Err(e),
        };
        match started {
            Ok(()) => Ok(guest),
            Err(e) => {
                let error = GuestBootError(format!("{e:#}"));
                guest.teardown(units).await;
                Err(error)
            }
        }
    }

    pub fn id(&self) -> &GuestId {
        &self.state.guest_id
    }

    pub fn state(&self) -> &GuestState {
        &self.state
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn vsock_socket(&self) -> PathBuf {
        self.dir.join("vsock.sock")
    }

    pub async fn resume(&mut self) -> Result<()> {
        self.api.vm_resume().await
    }

    pub async fn plug(&mut self, plan: &PlugPlan) -> Result<PluggedDevices> {
        let mut plugged = PluggedDevices {
            groups: device_groups(&plan.units),
            ..PluggedDevices::default()
        };
        self.state.devices_held = plan
            .units
            .iter()
            .map(|(class, unit)| HeldDevice {
                class: class.clone(),
                unit: unit.key(),
            })
            .collect();
        self.save()?;

        self.plug_devices(&plan.units, &mut plugged).await?;
        self.plug_mounts(plan).await?;
        if let Some(network) = &plan.network {
            self.plug_network(network).await?;
        }
        Ok(plugged)
    }

    pub fn record_job<T: Serialize>(&mut self, job: &T) -> Result<()> {
        self.state.job = Some(serde_json::to_value(job)?);
        self.save()
    }

    pub async fn teardown(mut self, units: &GuestUnits) {
        let vfio = self
            .state
            .devices_held
            .iter()
            .filter(|held| held.unit.starts_with("vfio:"))
            .count();
        for n in 0..vfio {
            quiet(
                self.api.vm_remove_device(&vfio_id(n)).await,
                "remove a VFIO device",
            );
        }
        quiet(self.api.vm_shutdown().await, "shut down the guest");
        for bus_id in std::mem::take(&mut self.state.usb_bus_ids) {
            if let Err(e) = self.request(SupervisorRequest::UnbindUsb { bus_id }).await {
                warn!(guest = %self.state.guest_id, error = %e, "could not unbind a USB device");
            }
        }
        quiet(self.api.vmm_shutdown().await, "stop cloud-hypervisor");
        self.control = None;
        let stopped = match units.stop(&self.state.guest_id).await {
            Ok(()) => true,
            Err(e) => {
                warn!(guest = %self.state.guest_id, error = %e, "could not stop the guest unit");
                false
            }
        };
        remove_guest_dirs(&self.dir, &self.state.per_job_dirs, stopped);
    }

    async fn start_unit(&self, units: &GuestUnits, config: &VmConfig) -> Result<()> {
        std::fs::create_dir_all(&self.dir)
            .with_context(|| format!("create {}", self.dir.display()))?;
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o2770))?;
        self.save()?;

        let mut argv = config.supervisor_command.clone();
        argv.push(self.dir.display().to_string());
        units.start(&self.state.guest_id, argv).await
    }

    async fn start_vm(&mut self, image: &GuestImage) -> Result<()> {
        wait_for_file(&self.dir.join("api.sock")).await?;
        wait_for_file(&self.dir.join("store.sock")).await?;

        self.api
            .vm_create(&vm_config(image, &self.dir, self.state.cid))
            .await?;
        self.api.vm_boot().await?;
        drop(wait_ready(&self.vsock_socket(), GUEST_BOOT_TIMEOUT).await?);
        self.api.vm_pause().await
    }

    async fn plug_devices(
        &mut self,
        units: &[(String, DeviceUnit)],
        plugged: &mut PluggedDevices,
    ) -> Result<()> {
        let mut vfio = 0;
        let mut next_port = AGENT_RELAY_PORT_BASE;
        for (class, unit) in units {
            let handle = match unit {
                DeviceUnit::Vfio(bdf) => {
                    let path = format!("/sys/bus/pci/devices/{bdf}/");
                    let info = self.api.vm_add_device(&path, &vfio_id(vfio)).await?;
                    vfio += 1;
                    info.bdf
                }
                DeviceUnit::Kvm => "/dev/kvm".to_owned(),
                DeviceUnit::Dev(path) => {
                    let name = relay_name(class, &plugged.relays);
                    let guest_port = next_port;
                    next_port += 1;
                    self.request(SupervisorRequest::RelayDevice {
                        path: path.display().to_string(),
                        guest_port,
                    })
                    .await?;
                    plugged.relays.push(GuestRelay {
                        name: name.clone(),
                        port: guest_port,
                        kind: GuestRelayKind::Pty,
                    });
                    format!("/dev/gradient/{name}")
                }
                DeviceUnit::Usb(vendor_product) => {
                    let reply = self
                        .request(SupervisorRequest::BindUsb {
                            vendor_product: vendor_product.clone(),
                        })
                        .await?;
                    let SupervisorReply::UsbBound { bus_id } = reply else {
                        bail!("guest supervisor answered {reply:?} to a USB bind");
                    };
                    self.state.usb_bus_ids.push(bus_id.clone());
                    self.save()?;
                    plugged.usb_bus_ids.push(bus_id.clone());
                    bus_id
                }
            };
            plugged
                .guest_devices
                .entry(class.clone())
                .or_default()
                .push(handle);
        }

        if !plugged.usb_bus_ids.is_empty() {
            self.request(SupervisorRequest::RelayUsbip {
                guest_port: next_port,
            })
            .await?;
            plugged.relays.push(GuestRelay {
                name: "usbip".to_owned(),
                port: next_port,
                kind: GuestRelayKind::Tcp { listen: USBIP_PORT },
            });
        }
        self.state.relays = plugged.relays.iter().map(StoredRelay::from).collect();
        self.save()
    }

    async fn plug_mounts(&mut self, plan: &PlugPlan) -> Result<()> {
        self.add_mount(WORK_MOUNT_TAG, &plan.work, false).await?;
        for (mount, dir) in &plan.mounts {
            if mount.per_job {
                std::fs::create_dir_all(dir)
                    .with_context(|| format!("create {}", dir.display()))?;
                self.state.per_job_dirs.push(dir.clone());
                self.save()?;
            }
            self.add_mount(&mount.name, dir, mount.read_only).await?;
            self.state.mounts.push(mount.name.clone());
            self.save()?;
        }
        Ok(())
    }

    async fn add_mount(&mut self, tag: &str, dir: &Path, read_only: bool) -> Result<()> {
        let reply = self
            .request(SupervisorRequest::PrepareMount {
                tag: tag.to_owned(),
                dir: dir.display().to_string(),
                read_only,
            })
            .await?;
        let SupervisorReply::MountSocket { path } = reply else {
            bail!("guest supervisor answered {reply:?} to a mount");
        };
        self.api
            .vm_add_fs(tag, Path::new(&path), &format!("fs-{tag}"))
            .await?;
        Ok(())
    }

    async fn plug_network(&mut self, network: &GuestNetwork) -> Result<()> {
        let cid = self.state.cid;
        let tap = format!("gr-tap-{cid}");
        self.request(SupervisorRequest::PrepareTap {
            name: tap.clone(),
            bridge: self.bridge.clone(),
        })
        .await?;
        self.state.tap = Some(tap.clone());
        self.save()?;
        let mac = format!(
            "02:47:52:{:02x}:{:02x}:{:02x}",
            (cid >> 16) & 0xff,
            (cid >> 8) & 0xff,
            cid & 0xff
        );
        self.api.vm_add_net(&tap, &mac, "net0").await?;
        self.state.address = Some(network.address.clone());
        self.save()
    }

    async fn request(&mut self, request: SupervisorRequest) -> Result<SupervisorReply> {
        let control = match &mut self.control {
            Some(control) => control,
            None => {
                let socket = self.dir.join("control.sock");
                let stream = UnixStream::connect(&socket)
                    .await
                    .with_context(|| format!("connect to {}", socket.display()))?;
                self.control.insert(GuestConnection::agree(stream).await?)
            }
        };
        control.send(&request).await?;
        match control.recv::<SupervisorReply>().await? {
            Some(SupervisorReply::Failed { message }) => Err(anyhow!(message)),
            Some(reply) => Ok(reply),
            None => {
                self.control = None;
                bail!("guest supervisor closed the control socket")
            }
        }
    }

    fn save(&self) -> Result<()> {
        write_state(&self.dir, &self.state)
    }
}

pub fn guest_address(subnet: &str, cid: u32) -> Result<GuestNetwork> {
    let (network, prefix) = subnet
        .split_once('/')
        .ok_or_else(|| anyhow!("subnet {subnet} has no prefix length"))?;
    let network: Ipv4Addr = network
        .parse()
        .with_context(|| format!("subnet {subnet}"))?;
    let prefix: u8 = prefix.parse().with_context(|| format!("subnet {subnet}"))?;
    if !(8..=30).contains(&prefix) {
        bail!("subnet {subnet} needs a prefix length between 8 and 30");
    }
    let mask = u32::MAX << (32 - prefix);
    let base = u32::from(network) & mask;
    let hosts = !mask;
    if cid >= hosts {
        bail!("subnet {subnet} has no address for guest cid {cid}");
    }
    Ok(GuestNetwork {
        address: Ipv4Addr::from(base + cid).to_string(),
        prefix,
        gateway: Ipv4Addr::from(base + 1).to_string(),
    })
}

pub fn device_groups(units: &[(String, DeviceUnit)]) -> Vec<String> {
    let groups: BTreeSet<&str> = units
        .iter()
        .flat_map(|(_, unit)| match unit {
            DeviceUnit::Kvm => &["kvm"][..],
            DeviceUnit::Vfio(_) => &["video", "render"][..],
            DeviceUnit::Dev(_) | DeviceUnit::Usb(_) => &["dialout"][..],
        })
        .copied()
        .collect();
    groups.into_iter().map(str::to_owned).collect()
}

pub fn remove_guest_dirs(dir: &Path, per_job_dirs: &[PathBuf], unit_stopped: bool) {
    if !unit_stopped {
        warn!(dir = %dir.display(), "keeping the guest directory of a unit that did not stop");
        return;
    }
    for dir in per_job_dirs.iter().map(PathBuf::as_path).chain([dir]) {
        if let Err(e) = std::fs::remove_dir_all(dir)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            warn!(dir = %dir.display(), error = %e, "could not remove a guest directory");
        }
    }
}

fn relay_name(class: &str, relays: &[GuestRelay]) -> String {
    let taken = |name: &str| relays.iter().any(|relay| relay.name == name);
    if !taken(class) {
        return class.to_owned();
    }
    (1..)
        .map(|n| format!("{class}-{n}"))
        .find(|name| !taken(name))
        .unwrap_or_else(|| class.to_owned())
}

fn vfio_id(n: usize) -> String {
    format!("vfio-{n}")
}

async fn wait_for_file(path: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + SOCKET_WAIT;
    while !tokio::fs::try_exists(path).await.unwrap_or(false) {
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "{} did not appear within {}s",
                path.display(),
                SOCKET_WAIT.as_secs()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

fn quiet(result: Result<()>, what: &str) {
    if let Err(e) = result {
        debug!(error = %e, "could not {what}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_guest_address_follows_its_cid() {
        assert_eq!(
            guest_address("10.233.0.0/16", 3).unwrap(),
            GuestNetwork {
                address: "10.233.0.3".into(),
                prefix: 16,
                gateway: "10.233.0.1".into(),
            }
        );
    }

    #[test]
    fn the_state_file_stays_while_the_unit_is_still_running() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("guest");
        let per_job = root.path().join("job");
        std::fs::create_dir(&dir).unwrap();
        std::fs::create_dir(&per_job).unwrap();
        std::fs::write(dir.join("state.json"), "{}").unwrap();

        remove_guest_dirs(&dir, std::slice::from_ref(&per_job), false);
        assert!(dir.join("state.json").exists());
        assert!(per_job.exists());

        remove_guest_dirs(&dir, std::slice::from_ref(&per_job), true);
        assert!(!dir.exists());
        assert!(!per_job.exists());
    }

    #[test]
    fn device_groups_follow_the_plugged_kinds() {
        let units = [
            ("kvm".to_owned(), DeviceUnit::Kvm),
            (
                "gpu:h100".to_owned(),
                DeviceUnit::Vfio("0000:17:00.0".into()),
            ),
            (
                "gpu:h100".to_owned(),
                DeviceUnit::Vfio("0000:18:00.0".into()),
            ),
        ];
        assert_eq!(device_groups(&units), ["kvm", "render", "video"]);
    }
}
