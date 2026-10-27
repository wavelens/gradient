/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gradient_wire::guest::{GuestConnection, SupervisorReply, SupervisorRequest};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::vsock::connect_vsock;

const SYSFS_USB_DEVICES: &str = "/sys/bus/usb/devices";
const SYSFS_NET: &str = "/sys/class/net";
pub const GUEST_USER_ID: u32 = 1000;
const USBIPD: &str = "127.0.0.1:3240";
const SOCKET_WAIT: Duration = Duration::from_secs(10);
const RELAY_RETRY: Duration = Duration::from_secs(1);

pub fn run_supervisor(dir: &Path, cloud_hypervisor: &str, virtiofsd: &str) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(supervise(dir, cloud_hypervisor, virtiofsd))
}

async fn supervise(dir: &Path, cloud_hypervisor: &str, virtiofsd: &str) -> Result<()> {
    let host = Arc::new(Host::new(dir, virtiofsd));
    let store = spawn_virtiofsd(
        virtiofsd,
        &dir.join("store.sock"),
        Path::new("/nix/store"),
        true,
        &[],
    )?;
    let mut hypervisor = Command::new(cloud_hypervisor)
        .arg("--api-socket")
        .arg(format!("path={}", dir.join("api.sock").display()))
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("start {cloud_hypervisor}"))?;
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
    virtiofsd: String,
    children: Mutex<Vec<Child>>,
    taps: Mutex<Vec<String>>,
    usb_bus_ids: Mutex<Vec<String>>,
    relays: Mutex<JoinSet<()>>,
}

impl Host {
    fn new(dir: &Path, virtiofsd: &str) -> Self {
        Self {
            dir: dir.to_owned(),
            virtiofsd: virtiofsd.to_owned(),
            children: Mutex::default(),
            taps: Mutex::default(),
            usb_bus_ids: Mutex::default(),
            relays: Mutex::default(),
        }
    }

    async fn handle(&self, request: SupervisorRequest) -> SupervisorReply {
        let result = match request {
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
        };
        result.unwrap_or_else(|e| SupervisorReply::Failed {
            message: format!("{e:#}"),
        })
    }

    async fn prepare_mount(
        &self,
        tag: &str,
        shared: &Path,
        read_only: bool,
    ) -> Result<SupervisorReply> {
        if tag.is_empty() || tag.contains('/') || tag.starts_with('.') {
            bail!("invalid mount tag {tag:?}");
        }
        let mounts = self.dir.join("mounts");
        tokio::fs::create_dir_all(&mounts).await?;
        let socket = mounts.join(format!("{tag}.sock"));
        let worker = std::fs::metadata(&self.dir)
            .with_context(|| format!("read the owner of {}", self.dir.display()))?;
        let ids = owner_translation(worker.uid(), worker.gid());
        let child = spawn_virtiofsd(&self.virtiofsd, &socket, shared, read_only, &ids)?;
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
}
