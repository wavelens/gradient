/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use gradient_wire::guest::{GuestConnection, SupervisorReply, SupervisorRequest};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::config::SupervisorConfig;
use crate::guest::WORK_MOUNT_TAG;
use crate::vsock::connect_vsock;

const SYSFS_USB_DEVICES: &str = "/sys/bus/usb/devices";
const SYSFS_NET: &str = "/sys/class/net";
pub const GUEST_USER_ID: u32 = 1000;
pub const TAP_PREFIX: &str = "gr-tap-";
const MAX_INTERFACE_NAME: usize = 15;
const USBIPD: &str = "127.0.0.1:3240";
const SOCKET_WAIT: Duration = Duration::from_secs(10);
const RELAY_RETRY: Duration = Duration::from_secs(1);

pub fn run_supervisor(dir: &Path, config: SupervisorConfig) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(supervise(dir, config))
}

async fn supervise(dir: &Path, config: SupervisorConfig) -> Result<()> {
    let store = spawn_virtiofsd(
        &config.virtiofsd_bin,
        &dir.join("store.sock"),
        Path::new("/nix/store"),
        true,
        &[],
    )?;
    let cloud_hypervisor = &config.cloud_hypervisor_bin;
    let mut hypervisor = Command::new(cloud_hypervisor)
        .arg("--api-socket")
        .arg(format!("path={}", dir.join("api.sock").display()))
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("start {cloud_hypervisor}"))?;
    let host = Arc::new(Host::new(dir, config));
    let control = bind(&dir.join("control.sock"))?;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut sessions = JoinSet::new();

    let status = loop {
        tokio::select! {
            status = hypervisor.wait() => break status?,
            accepted = control.accept() => {
                let (stream, _) = accepted?;
                sessions.spawn(serve(Arc::clone(&host), stream));
            }
            _ = terminate.recv() => {
                info!("guest supervisor stopping cloud-hypervisor");
                hypervisor.start_kill()?;
            }
        }
    };

    sessions.abort_all();
    host.release().await;
    drop(store);
    if !status.success() {
        bail!("cloud-hypervisor exited with {status}");
    }
    Ok(())
}

struct Host {
    dir: PathBuf,
    config: SupervisorConfig,
    children: Mutex<Vec<Child>>,
    taps: Mutex<Vec<String>>,
    usb_bus_ids: Mutex<Vec<String>>,
    relays: Mutex<JoinSet<()>>,
}

impl Host {
    fn new(dir: &Path, config: SupervisorConfig) -> Self {
        Self {
            dir: dir.to_owned(),
            config,
            children: Mutex::default(),
            taps: Mutex::default(),
            usb_bus_ids: Mutex::default(),
            relays: Mutex::default(),
        }
    }

    async fn handle(&self, request: SupervisorRequest) -> SupervisorReply {
        let bound_usb = lock(&self.usb_bus_ids).clone();
        let result = match check_request(&self.config, &self.dir, &bound_usb, &request) {
            Ok(()) => self.serve_request(request).await,
            Err(e) => Err(e),
        };
        result.unwrap_or_else(|e| SupervisorReply::Failed {
            message: format!("{e:#}"),
        })
    }

    async fn serve_request(&self, request: SupervisorRequest) -> Result<SupervisorReply> {
        match request {
            SupervisorRequest::PrepareMount {
                tag,
                dir,
                read_only,
            } => self.prepare_mount(&tag, Path::new(&dir), read_only).await,
            SupervisorRequest::PrepareTap { name, bridge } => {
                self.prepare_tap(&name, &bridge).await
            }
            SupervisorRequest::BindUsb { vendor_product } => self.bind_usb(&vendor_product).await,
            SupervisorRequest::UnbindUsb { bus_id } => self.unbind_usb(&bus_id).await,
            SupervisorRequest::RelayDevice { path, guest_port } => {
                self.relay(guest_port, HostEnd::File(PathBuf::from(path)))
            }
            SupervisorRequest::RelayUsbip { guest_port } => self.relay(guest_port, HostEnd::Usbipd),
        }
    }

    async fn prepare_mount(
        &self,
        tag: &str,
        shared: &Path,
        read_only: bool,
    ) -> Result<SupervisorReply> {
        let shared_type = std::fs::symlink_metadata(shared)
            .with_context(|| format!("inspect {}", shared.display()))?
            .file_type();
        ensure!(
            shared_type.is_dir(),
            "{} is not a directory",
            shared.display()
        );
        let mounts = self.dir.join("mounts");
        tokio::fs::create_dir_all(&mounts).await?;
        let socket = mounts.join(format!("{tag}.sock"));
        let worker = std::fs::metadata(&self.dir)
            .with_context(|| format!("read the owner of {}", self.dir.display()))?;
        let ids = owner_translation(worker.uid(), worker.gid());
        let child = spawn_virtiofsd(&self.config.virtiofsd_bin, &socket, shared, read_only, &ids)?;
        lock(&self.children).push(child);
        wait_for_socket(&socket).await?;
        Ok(SupervisorReply::MountSocket {
            path: socket.display().to_string(),
        })
    }

    async fn prepare_tap(&self, name: &str, bridge: &str) -> Result<SupervisorReply> {
        if Path::new(SYSFS_NET).join(name).exists() {
            run("ip", &["link", "del", "dev", name]).await?;
        }
        run("ip", &["tuntap", "add", "dev", name, "mode", "tap"]).await?;
        lock(&self.taps).push(name.to_owned());
        run("ip", &["link", "set", name, "master", bridge, "up"]).await?;
        Ok(SupervisorReply::Done)
    }

    async fn bind_usb(&self, vendor_product: &str) -> Result<SupervisorReply> {
        let Some(bus_id) = usb_bus_id(Path::new(SYSFS_USB_DEVICES), vendor_product) else {
            bail!("no USB device {vendor_product} on the host");
        };
        run("usbip", &["bind", "-b", &bus_id]).await?;
        lock(&self.usb_bus_ids).push(bus_id.clone());
        Ok(SupervisorReply::UsbBound { bus_id })
    }

    async fn unbind_usb(&self, bus_id: &str) -> Result<SupervisorReply> {
        run("usbip", &["unbind", "-b", bus_id]).await?;
        lock(&self.usb_bus_ids).retain(|bound| bound != bus_id);
        Ok(SupervisorReply::Done)
    }

    fn relay(&self, guest_port: u32, host_end: HostEnd) -> Result<SupervisorReply> {
        let vsock = self.dir.join("vsock.sock");
        lock(&self.relays).spawn(relay_loop(vsock, guest_port, host_end));
        Ok(SupervisorReply::Done)
    }

    async fn release(&self) {
        lock(&self.relays).abort_all();
        let bus_ids = std::mem::take(&mut *lock(&self.usb_bus_ids));
        for bus_id in bus_ids {
            if let Err(e) = run("usbip", &["unbind", "-b", &bus_id]).await {
                warn!(%bus_id, error = %e, "could not unbind a USB device");
            }
        }
        let taps = std::mem::take(&mut *lock(&self.taps));
        for tap in taps {
            if let Err(e) = run("ip", &["link", "del", &tap]).await {
                warn!(%tap, error = %e, "could not delete a tap");
            }
        }
        lock(&self.children).clear();
    }
}

async fn serve(host: Arc<Host>, stream: UnixStream) {
    let session = async {
        let mut conn = GuestConnection::agree(stream).await?;
        while let Some(request) = conn.recv::<SupervisorRequest>().await? {
            let reply = host.handle(request).await;
            conn.send(&reply).await?;
        }
        anyhow::Ok(())
    };
    if let Err(e) = session.await {
        warn!(error = %e, "guest control session ended");
    }
}

enum HostEnd {
    File(PathBuf),
    Usbipd,
}

async fn relay_loop(vsock: PathBuf, guest_port: u32, host_end: HostEnd) {
    loop {
        if let Err(e) = relay_once(&vsock, guest_port, &host_end).await {
            warn!(guest_port, error = %e, "device relay interrupted");
        }
        tokio::time::sleep(RELAY_RETRY).await;
    }
}

async fn relay_once(vsock: &Path, guest_port: u32, host_end: &HostEnd) -> Result<()> {
    let mut guest = connect_vsock(vsock, guest_port).await?;
    match host_end {
        HostEnd::File(path) => {
            let mut file = tokio::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .await
                .with_context(|| format!("open {}", path.display()))?;
            tokio::io::copy_bidirectional(&mut guest, &mut file).await?;
        }
        HostEnd::Usbipd => {
            let mut usbipd = TcpStream::connect(USBIPD).await?;
            tokio::io::copy_bidirectional(&mut guest, &mut usbipd).await?;
        }
    }
    Ok(())
}

pub fn check_request(
    config: &SupervisorConfig,
    dir: &Path,
    bound_usb: &[String],
    request: &SupervisorRequest,
) -> Result<()> {
    let usb_configured = |vendor_product: &str| {
        config
            .devices
            .iter()
            .any(|device| device.usb.as_deref() == Some(vendor_product))
    };
    match request {
        SupervisorRequest::PrepareMount {
            tag,
            dir: shared,
            read_only,
        } => check_mount(config, dir, tag, Path::new(shared), *read_only),
        SupervisorRequest::PrepareTap { name, bridge } => {
            ensure!(
                *bridge == config.bridge,
                "bridge {bridge:?} is not the guest bridge"
            );
            ensure!(is_tap_name(name), "{name:?} is not a guest tap name");
            Ok(())
        }
        SupervisorRequest::BindUsb { vendor_product } => {
            ensure!(
                usb_configured(vendor_product),
                "USB device {vendor_product} is not configured"
            );
            Ok(())
        }
        SupervisorRequest::UnbindUsb { bus_id } => {
            ensure!(
                bound_usb.contains(bus_id),
                "USB device {bus_id} is not bound for this guest"
            );
            Ok(())
        }
        SupervisorRequest::RelayDevice { path, .. } => {
            let configured = config
                .devices
                .iter()
                .any(|device| device.dev.as_deref() == Some(Path::new(path)));
            ensure!(configured, "device {path} is not configured");
            Ok(())
        }
        SupervisorRequest::RelayUsbip { .. } => {
            ensure!(
                !bound_usb.is_empty(),
                "no USB device is bound for this guest"
            );
            Ok(())
        }
    }
}

fn check_mount(
    config: &SupervisorConfig,
    dir: &Path,
    tag: &str,
    shared: &Path,
    read_only: bool,
) -> Result<()> {
    if tag == WORK_MOUNT_TAG {
        ensure!(
            shared == dir.join("work"),
            "{} is not the guest work directory",
            shared.display()
        );
        return Ok(());
    }
    let Some(mount) = config.mounts.iter().find(|mount| mount.name == tag) else {
        bail!("mount {tag:?} is not configured");
    };
    ensure!(read_only || !mount.read_only, "mount {tag:?} is read-only");
    let allowed = if mount.per_job {
        shared.parent() == Some(mount.path.as_path())
            && matches!(shared.components().next_back(), Some(Component::Normal(_)))
    } else {
        shared == mount.path
    };
    ensure!(
        allowed,
        "{} is not a directory of mount {tag:?}",
        shared.display()
    );
    Ok(())
}

fn is_tap_name(name: &str) -> bool {
    let cid = name.strip_prefix(TAP_PREFIX).unwrap_or_default();
    name.len() <= MAX_INTERFACE_NAME && !cid.is_empty() && cid.bytes().all(|b| b.is_ascii_digit())
}

pub fn usb_bus_id(sysfs_usb_devices: &Path, vendor_product: &str) -> Option<String> {
    let (vendor, product) = vendor_product.split_once(':')?;
    let mut entries: Vec<_> = std::fs::read_dir(sysfs_usb_devices)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    entries.into_iter().find_map(|device| {
        let read = |name: &str| std::fs::read_to_string(device.join(name)).ok();
        if read("idVendor")?.trim() != vendor || read("idProduct")?.trim() != product {
            return None;
        }
        device.file_name()?.to_str().map(str::to_owned)
    })
}

fn owner_translation(worker_uid: u32, worker_gid: u32) -> [String; 4] {
    let all = u32::MAX;
    [
        format!("--translate-uid=squash-guest:0:{worker_uid}:{all}"),
        format!("--translate-uid=host:{worker_uid}:{GUEST_USER_ID}:1"),
        format!("--translate-gid=squash-guest:0:{worker_gid}:{all}"),
        format!("--translate-gid=host:{worker_gid}:{GUEST_USER_ID}:1"),
    ]
}

fn spawn_virtiofsd(
    bin: &str,
    socket: &Path,
    shared: &Path,
    read_only: bool,
    ids: &[String],
) -> Result<Child> {
    let _ = std::fs::remove_file(socket);
    let mut command = Command::new(bin);
    command
        .arg(format!("--socket-path={}", socket.display()))
        .arg(format!("--shared-dir={}", shared.display()))
        .arg("--sandbox=none")
        .arg("--cache=auto")
        .args(ids)
        .kill_on_drop(true);
    if read_only {
        command.arg("--readonly");
    }
    command
        .spawn()
        .with_context(|| format!("start {bin} for {}", shared.display()))
}

fn bind(socket: &Path) -> Result<UnixListener> {
    let _ = std::fs::remove_file(socket);
    UnixListener::bind(socket).with_context(|| format!("listen on {}", socket.display()))
}

async fn wait_for_socket(socket: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + SOCKET_WAIT;
    while !tokio::fs::try_exists(socket).await.unwrap_or(false) {
        if tokio::time::Instant::now() >= deadline {
            bail!("{} did not appear", socket.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

async fn run(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .await
        .with_context(|| format!("run {program}"))?;
    if !output.status.success() {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DeviceConfig, MountConfig};

    fn usb_device(root: &Path, name: &str, vendor: &str, product: Option<&str>) {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("idVendor"), format!("{vendor}\n")).unwrap();
        if let Some(product) = product {
            std::fs::write(dir.join("idProduct"), format!("{product}\n")).unwrap();
        }
    }

    #[test]
    fn a_usb_device_is_found_by_vendor_and_product() {
        let root = tempfile::tempdir().unwrap();
        usb_device(root.path(), "1-2", "1d6b", Some("0002"));
        usb_device(root.path(), "1-3", "046d", None);

        assert_eq!(usb_bus_id(root.path(), "1d6b:0002"), Some("1-2".to_owned()));
        assert_eq!(usb_bus_id(root.path(), "ffff:0001"), None);
    }

    fn worker_config() -> SupervisorConfig {
        let mount = |name: &str, read_only, per_job| MountConfig {
            name: name.to_owned(),
            path: PathBuf::from(format!("/srv/{name}")),
            read_only,
            per_job,
        };
        SupervisorConfig {
            cloud_hypervisor_bin: "cloud-hypervisor".to_owned(),
            virtiofsd_bin: "virtiofsd".to_owned(),
            devices: vec![
                DeviceConfig {
                    name: "serial".to_owned(),
                    vfio: Vec::new(),
                    dev: Some(PathBuf::from("/dev/ttyUSB0")),
                    usb: None,
                },
                DeviceConfig {
                    name: "camera".to_owned(),
                    vfio: Vec::new(),
                    dev: None,
                    usb: Some("1d6b:0002".to_owned()),
                },
            ],
            mounts: vec![
                mount("cache", false, false),
                mount("datasets", true, false),
                mount("scratch", false, true),
            ],
            bridge: "gradient-guests".to_owned(),
        }
    }

    fn mount(tag: &str, dir: &str, read_only: bool) -> SupervisorRequest {
        SupervisorRequest::PrepareMount {
            tag: tag.to_owned(),
            dir: dir.to_owned(),
            read_only,
        }
    }

    fn allowed(request: SupervisorRequest) -> bool {
        let bound = ["1-2".to_owned()];
        check_request(&worker_config(), Path::new("/guests/g1"), &bound, &request).is_ok()
    }

    #[test]
    fn only_configured_mount_directories_are_shared() {
        assert!(allowed(mount("work", "/guests/g1/work", false)));
        assert!(allowed(mount("cache", "/srv/cache", false)));
        assert!(allowed(mount("datasets", "/srv/datasets", true)));
        assert!(allowed(mount("scratch", "/srv/scratch/job-1", false)));

        assert!(!allowed(mount("work", "/etc", false)));
        assert!(!allowed(mount("cache", "/srv/cache/sub", false)));
        assert!(!allowed(mount("datasets", "/srv/datasets", false)));
        assert!(!allowed(mount("scratch", "/srv/scratch", false)));
        assert!(!allowed(mount("scratch", "/srv/scratch/..", false)));
        assert!(!allowed(mount("scratch", "/srv/scratch/a/b", false)));
        assert!(!allowed(mount("root", "/", false)));
    }

    #[test]
    fn only_guest_taps_on_the_guest_bridge_are_made() {
        let tap = |name: &str, bridge: &str| SupervisorRequest::PrepareTap {
            name: name.to_owned(),
            bridge: bridge.to_owned(),
        };
        assert!(allowed(tap("gr-tap-7", "gradient-guests")));
        assert!(!allowed(tap("gr-tap-7", "br-lan")));
        assert!(!allowed(tap("eth0", "gradient-guests")));
        assert!(!allowed(tap("gr-tap-", "gradient-guests")));
        assert!(!allowed(tap("gr-tap-123456789", "gradient-guests")));
    }

    #[test]
    fn only_configured_devices_are_handed_out() {
        let relay = |path: &str| SupervisorRequest::RelayDevice {
            path: path.to_owned(),
            guest_port: 7100,
        };
        let bind = |vendor_product: &str| SupervisorRequest::BindUsb {
            vendor_product: vendor_product.to_owned(),
        };
        let unbind = |bus_id: &str| SupervisorRequest::UnbindUsb {
            bus_id: bus_id.to_owned(),
        };
        assert!(allowed(relay("/dev/ttyUSB0")));
        assert!(!allowed(relay("/dev/sda")));
        assert!(allowed(bind("1d6b:0002")));
        assert!(!allowed(bind("046d:c52b")));
        assert!(allowed(unbind("1-2")));
        assert!(!allowed(unbind("3-1")));
    }
}
