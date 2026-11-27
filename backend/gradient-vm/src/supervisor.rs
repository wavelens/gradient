/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::net::{Ipv4Addr, Ipv6Addr};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail, ensure};
use gradient_wire::guest::{GuestConnection, SupervisorReply, SupervisorRequest};
use rustix::fs::{Gid, Mode, OFlags, Uid};
use tokio::io::AsyncReadExt;
use tokio::net::{TcpStream, UnixListener, UnixStream};
use tokio::process::{Child, Command};
use tokio::signal::unix::{SignalKind, signal};
use tokio::task::JoinSet;
use tracing::{info, warn};

use crate::addresses::derive_ipv6_address_from_ipv4_host_offset;
use crate::config::SupervisorConfig;
use crate::console::{CONSOLE_SOCKET, serve_console};
use crate::guest::guest_mac;
use crate::guest_dir::OpenedGuestDirectory;
use crate::state::GuestId;
use crate::vsock::{VSOCK_SOCKET, send_vsock_connect_line_and_wait_for_ok};

const SYSFS_USB_DEVICES: &str = "/sys/bus/usb/devices";
const SYSFS_NET: &str = "/sys/class/net";
pub const GUEST_USER_ID: u32 = 1000;
pub const TAP_PREFIX: &str = "gr-tap-";
const MAX_INTERFACE_NAME: usize = 15;
const USBIPD: &str = "127.0.0.1:3240";
const SOCKET_WAIT: Duration = Duration::from_secs(10);
const SHARED_SOCKET_MODE: u32 = 0o660;
const RELAY_RETRY: Duration = Duration::from_secs(1);
const TAP_RECORD_DIRECTORY_LOCK_WAIT_DURATION: Duration = Duration::from_secs(10);
const TAP_RECORD_DIRECTORY_LOCK_RETRY_SLEEP_DURATION: Duration = Duration::from_millis(20);
const HOST_COMMAND_TIMEOUT_DURATION: Duration = Duration::from_secs(5);
pub const GUEST_RUN_DIRECTORY_BASE_PATH: &str = "/run/gradient-guests";
pub const CONTROL_SOCKET: &str = "control.sock";
const CONFIGURED_HOST_DIRECTORY_WALK_START_PATH: &str = "/";
const REQUIRED_ANCESTOR_OWNER_USER_ID: u32 = 0;
// A worker module for NixOS can create this bridge table and its set of type ifname . ether_addr . ipv4_addr with the same name.
pub const GUEST_BRIDGE_TABLE_NAME: &str = "gradient-guests";
pub const GUEST_TAP_MAC_IPV4_NFTABLES_SET_NAME: &str = "guest-tap-addresses";
pub const GUEST_TAP_IPV6_NFTABLES_SET_NAME: &str = "guest-tap-ipv6-addresses";
pub const GUEST_INET_FILTER_TABLE_NAME: &str = "gradient-guests";
pub const GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH: &str =
    "/run/gradient-guest-tap-addresses";

#[derive(Debug)]
pub struct GuestSupervisorHostConfigurationError(pub String);

impl fmt::Display for GuestSupervisorHostConfigurationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for GuestSupervisorHostConfigurationError {}

pub fn run_supervisor(dir: &Path, config: SupervisorConfig) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(supervise(dir, config))
}

async fn supervise(dir: &Path, config: SupervisorConfig) -> Result<()> {
    let worker_guest_directory = Arc::new(
        OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner(dir)?,
    );
    let guest_id: GuestId = dir
        .file_name()
        .and_then(|name| name.to_str())
        .with_context(|| {
            format!(
                "no guest id in the last path component of {}",
                dir.display()
            )
        })?
        .parse()
        .map_err(anyhow::Error::msg)?;
    let guest_supervisor_directory_path = guest_supervisor_directory_path(&guest_id);
    let supervisor_directory_stat = rustix::fs::open(
        &guest_supervisor_directory_path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .and_then(rustix::fs::fstat)
    .with_context(|| {
        format!(
            "Read the owner and mode of `{}`.",
            guest_supervisor_directory_path.display()
        )
    })?;
    check_directory_has_required_owner_and_no_group_or_other_write(
        &guest_supervisor_directory_path,
        supervisor_directory_stat.st_uid,
        supervisor_directory_stat.st_mode,
        REQUIRED_ANCESTOR_OWNER_USER_ID,
    )?;
    let host = Arc::new(Host::new(
        guest_id,
        Arc::clone(&worker_guest_directory),
        config,
    ));
    let control = bind(&guest_supervisor_directory_path.join(CONTROL_SOCKET))?;
    let console_clients = bind(&guest_supervisor_directory_path.join(CONSOLE_SOCKET))?;
    let console = serve_console(
        &guest_supervisor_directory_path,
        &worker_guest_directory,
        console_clients,
    );
    tokio::pin!(console);
    let mut console_served = true;
    let mut terminate = signal(SignalKind::terminate())?;
    let mut sessions = JoinSet::new();

    let served = loop {
        tokio::select! {
            served = &mut console, if console_served => {
                console_served = false;
                if let Err(e) = served {
                    warn!(error = %format!("{e:#}"), "the guest console relay stopped");
                }
            }
            accepted = control.accept() => match accepted {
                Ok((stream, _)) => {
                    sessions.spawn(serve(Arc::clone(&host), stream));
                }
                Err(e) => break Err(e),
            },
            _ = terminate.recv() => break Ok(()),
        }
    };

    sessions.abort_all();
    host.release().await;
    info!(
        "The guest supervisor released the USB devices of the guest. Cleanup steps after unit stop release that tap."
    );
    served.context("Guest Control Socket Failure")
}

struct Host {
    guest_id: GuestId,
    worker_guest_directory: Arc<OpenedGuestDirectory>,
    config: SupervisorConfig,
    children: Mutex<Vec<Child>>,
    usb_bus_ids: Mutex<Vec<String>>,
    relays: Mutex<JoinSet<()>>,
}

impl Host {
    fn new(
        guest_id: GuestId,
        worker_guest_directory: Arc<OpenedGuestDirectory>,
        config: SupervisorConfig,
    ) -> Self {
        Self {
            guest_id,
            worker_guest_directory,
            config,
            children: Mutex::default(),
            usb_bus_ids: Mutex::default(),
            relays: Mutex::default(),
        }
    }

    async fn handle(&self, request: SupervisorRequest) -> SupervisorReply {
        let bound_usb = lock(&self.usb_bus_ids).clone();
        let result = match check_request(&self.config, &bound_usb, &request) {
            Ok(()) => self.serve_request(request).await,
            Err(e) => Err(e),
        };
        result.unwrap_or_else(supervisor_request_error_to_reply)
    }

    async fn serve_request(&self, request: SupervisorRequest) -> Result<SupervisorReply> {
        match request {
            SupervisorRequest::PrepareMount { tag, read_only } => {
                self.prepare_mount(&tag, read_only).await
            }
            SupervisorRequest::PrepareTap {
                name,
                bridge,
                guest_ipv4_address,
            } => {
                let Some(guest_ipv4_address) = guest_ipv4_address else {
                    bail!("tap {name:?} with no guest IPv4 address");
                };
                self.prepare_tap(&name, &bridge, guest_ipv4_address).await
            }
            SupervisorRequest::BindUsb { vendor_product } => self.bind_usb(&vendor_product).await,
            SupervisorRequest::UnbindUsb { bus_id } => self.unbind_usb(&bus_id).await,
            SupervisorRequest::RelayDevice { path, guest_port } => {
                self.relay(guest_port, HostEnd::File(PathBuf::from(path)))
            }
            SupervisorRequest::RelayUsbip { guest_port } => self.relay(guest_port, HostEnd::Usbipd),
        }
    }

    async fn prepare_mount(&self, tag: &str, read_only: bool) -> Result<SupervisorReply> {
        let shared = open_host_mount_directory_and_create_job_directory_owned_by_worker(
            &self.config,
            &self.worker_guest_directory,
            &self.guest_id,
            tag,
            Path::new(CONFIGURED_HOST_DIRECTORY_WALK_START_PATH),
            REQUIRED_ANCESTOR_OWNER_USER_ID,
        )?;
        let socket = guest_supervisor_directory_path(&self.guest_id).join(format!("{tag}.sock"));
        let ids = owner_translation(
            self.worker_guest_directory.guest_directory_owner_user_id(),
            self.worker_guest_directory.worker_group_id(),
        );
        let child = spawn_virtiofsd(
            &self.config.virtiofsd_bin,
            &socket,
            tag,
            &shared,
            read_only,
            &ids,
        )?;
        lock(&self.children).push(child);
        wait_for_socket(&socket).await?;
        share(&socket, self.worker_guest_directory.worker_group_id())?;
        Ok(SupervisorReply::MountSocket {
            path: socket.display().to_string(),
        })
    }

    async fn prepare_tap(
        &self,
        name: &str,
        bridge: &str,
        guest_ipv4_address: Ipv4Addr,
    ) -> Result<SupervisorReply> {
        let created_tap_guest_address =
            CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                name,
                guest_ipv4_address,
                derive_guest_ipv6_address_from_supervisor_config_and_ipv4_address(
                    &self.config,
                    guest_ipv4_address,
                )?,
            )?;
        run("nft", &nft_list_inet_guest_filter_table_arguments())
            .await
            .context("The nftables table `inet gradient-guests` does not exist. The traffic filter needs that table, so the guest stays stopped. Restore the table with `systemctl reload gradient-guest-nftables`.")?;
        let locked_tap_record_directory_file = lock_tap_record_directory().await?;
        if let Err(e) =
            release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
                &RealHostCommand,
                Path::new(GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH),
                Path::new(GUEST_RUN_DIRECTORY_BASE_PATH),
                None,
            )
            .await
        {
            warn!(error = %format!("{e:#}"), "Failed to release the taps of guests that no longer run. A retry runs at the next start or stop of any guest on the host.");
        }
        delete_leftover_tap_record_nft_elements_file_and_device_for_tap_name(
            &RealHostCommand,
            Path::new(GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH),
            name,
        )
        .await?;
        write_tap_record_then_create_tap_device(
            &RealHostCommand,
            Path::new(GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH),
            &TapAddressRecordWithGuestId {
                tap_guest_address: created_tap_guest_address,
                guest_id: self.guest_id.clone(),
            },
            bridge,
            (
                &self
                    .worker_guest_directory
                    .guest_directory_owner_user_id()
                    .to_string(),
                &self.worker_guest_directory.worker_group_id().to_string(),
            ),
            self.config.guest_bridge_ipv6_subnet.is_some(),
        )
        .await?;
        drop(locked_tap_record_directory_file);
        Ok(SupervisorReply::Done)
    }

    async fn bind_usb(&self, vendor_product: &str) -> Result<SupervisorReply> {
        let Some(bus_id) = usb_bus_id(Path::new(SYSFS_USB_DEVICES), vendor_product) else {
            bail!("no USB device {vendor_product} on the host");
        };
        if usb_device_is_bound_to_usbip_host_driver(Path::new(SYSFS_USB_DEVICES), &bus_id) {
            run("usbip", &["unbind", "-b", &bus_id]).await?;
        }
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
        lock(&self.relays).spawn(relay_loop(
            Arc::clone(&self.worker_guest_directory),
            guest_port,
            host_end,
        ));
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

async fn relay_loop(
    worker_guest_directory: Arc<OpenedGuestDirectory>,
    guest_port: u32,
    host_end: HostEnd,
) {
    loop {
        if let Err(e) = relay_once(&worker_guest_directory, guest_port, &host_end).await {
            warn!(guest_port, error = %e, "device relay interrupted");
        }
        tokio::time::sleep(RELAY_RETRY).await;
    }
}

async fn relay_once(
    worker_guest_directory: &OpenedGuestDirectory,
    guest_port: u32,
    host_end: &HostEnd,
) -> Result<()> {
    let stream = worker_guest_directory
        .connect_to_worker_owned_socket_in_guest_directory_without_following_symlinks(VSOCK_SOCKET)
        .await?;
    let mut guest = send_vsock_connect_line_and_wait_for_ok(stream, guest_port).await?;
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
        SupervisorRequest::PrepareMount { tag, read_only } => check_mount(config, tag, *read_only),
        SupervisorRequest::PrepareTap {
            name,
            bridge,
            guest_ipv4_address,
        } => {
            ensure!(
                *bridge == config.bridge,
                "bridge {bridge:?} is not the guest bridge"
            );
            ensure!(is_tap_name(name), "{name:?} is not a guest tap name");
            let Some(guest_ipv4_address) = guest_ipv4_address else {
                bail!("tap {name:?} with no guest IPv4 address");
            };
            check_guest_ipv4_address_is_usable_host_address_in_guest_subnet(
                config,
                *guest_ipv4_address,
            )
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

fn check_guest_ipv4_address_is_usable_host_address_in_guest_subnet(
    config: &SupervisorConfig,
    guest_ipv4_address: Ipv4Addr,
) -> Result<()> {
    let Some(subnet) = config.guest_bridge_ipv4_subnet else {
        bail!("exec.subnet.v4 does not set a guest subnet.");
    };
    ensure!(
        subnet.contains(&guest_ipv4_address),
        "guest address {address} outside the range of guest subnet {subnet}",
        address = guest_ipv4_address
    );
    let guest_subnet_gateway_address = subnet.hosts().next().unwrap_or(subnet.network());
    ensure!(
        ![
            subnet.network(),
            subnet.broadcast(),
            guest_subnet_gateway_address
        ]
        .contains(&guest_ipv4_address),
        "guest address {address} is the network address, broadcast address, or bridge address of {subnet}",
        address = guest_ipv4_address
    );
    Ok(())
}

fn derive_guest_ipv6_address_from_supervisor_config_and_ipv4_address(
    config: &SupervisorConfig,
    guest_ipv4_address: Ipv4Addr,
) -> Result<Option<Ipv6Addr>> {
    let (Some(guest_bridge_ipv4_subnet), Some(guest_bridge_ipv6_subnet)) = (
        config.guest_bridge_ipv4_subnet,
        config.guest_bridge_ipv6_subnet,
    ) else {
        return Ok(None);
    };
    let Some(guest_ipv6_address) = derive_ipv6_address_from_ipv4_host_offset(
        guest_bridge_ipv4_subnet,
        guest_bridge_ipv6_subnet,
        guest_ipv4_address,
    ) else {
        bail!(
            "The guest address {guest_ipv4_address} is outside the guest subnet {guest_bridge_ipv4_subnet}. No IPv6 address in {guest_bridge_ipv6_subnet} can exist for that guest address. The supervisor cannot create a tap."
        );
    };
    Ok(Some(guest_ipv6_address))
}

fn check_mount(config: &SupervisorConfig, tag: &str, read_only: bool) -> Result<()> {
    let Some(mount) = config.mounts.iter().find(|mount| mount.name == tag) else {
        bail!("mount {tag:?} is not configured");
    };
    ensure!(read_only || !mount.read_only, "mount {tag:?} is read-only");
    Ok(())
}

fn open_host_mount_directory_and_create_job_directory_owned_by_worker(
    config: &SupervisorConfig,
    worker_guest_directory: &OpenedGuestDirectory,
    guest_id: &GuestId,
    tag: &str,
    trusted_walk_start_directory_path: &Path,
    required_ancestor_owner_user_id: u32,
) -> Result<OwnedFd> {
    let worker_user_id = worker_guest_directory.guest_directory_owner_user_id();
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let Some(mount) = config.mounts.iter().find(|mount| mount.name == tag) else {
        bail!("mount {tag:?} is not configured");
    };
    let mount_path =
        open_directory_path_from_trusted_start_without_symlinks_requiring_owned_ancestors(
            trusted_walk_start_directory_path,
            &mount.path,
            required_ancestor_owner_user_id,
        )
        .with_context(|| format!("Open the configured path for mount {tag:?}"))?;
    if !mount.per_job {
        return Ok(mount_path);
    }
    let name = guest_id.to_string();
    let job_mount_directory_created =
        match rustix::fs::mkdirat(&mount_path, &name, Mode::from_bits_truncate(0o750)) {
            Ok(()) => true,
            Err(rustix::io::Errno::EXIST) => false,
            Err(e) => {
                return Err(
                    anyhow::Error::from(e).context(format!("Create {name} in mount {tag:?}"))
                );
            }
        };
    let job_directory = rustix::fs::openat(&mount_path, &name, directory_flags, Mode::empty())
        .with_context(|| format!("Open {name} in the {tag:?} mount without following symlinks."))?;
    let owner = rustix::fs::fstat(&job_directory)
        .with_context(|| format!("Read the owner of {name} in mount {tag:?}"))?
        .st_uid;
    can_hand_job_directory_to_worker_user(
        &mount.host_directory_for_guest(guest_id),
        job_mount_directory_created,
        owner,
        worker_user_id,
        rustix::process::geteuid().as_raw(),
        opened_directory_has_no_entries(&job_directory)
            .with_context(|| format!("read the entries of {name} in mount {tag:?}"))?,
    )?;
    rustix::fs::fchown(
        &job_directory,
        Some(Uid::from_raw(worker_user_id)),
        Some(Gid::from_raw(worker_guest_directory.worker_group_id())),
    )
    .with_context(|| format!("Assign {name} in mount {tag:?} to the worker user"))?;
    Ok(job_directory)
}

fn can_hand_job_directory_to_worker_user(
    directory: &Path,
    job_mount_directory_created: bool,
    owner_user_id: u32,
    worker_user_id: u32,
    created_per_job_directory_owner_user_id: u32,
    job_directory_has_no_entries: bool,
) -> Result<(), GuestSupervisorHostConfigurationError> {
    let directory = directory.display();
    match job_mount_directory_created {
        false if owner_user_id != worker_user_id => {
            Err(GuestSupervisorHostConfigurationError(format!(
                "Refused: {directory} was already present. User {owner_user_id} owned {directory}, not worker user {worker_user_id}. Remove the directory."
            )))
        }
        true if owner_user_id != created_per_job_directory_owner_user_id => {
            Err(GuestSupervisorHostConfigurationError(format!(
                "Denied: the supervisor created {directory} moments ago, but the owner is user {owner_user_id}, not user {created_per_job_directory_owner_user_id}."
            )))
        }
        true if !job_directory_has_no_entries => Err(GuestSupervisorHostConfigurationError(
            format!("Rejected: {directory} is not empty right after creation."),
        )),
        _ => Ok(()),
    }
}

fn opened_directory_has_no_entries(directory: &OwnedFd) -> Result<bool> {
    for entry in rustix::fs::Dir::read_from(directory)? {
        if !matches!(entry?.file_name().to_bytes(), b"." | b"..") {
            return Ok(false);
        }
    }
    Ok(true)
}

fn is_tap_name(name: &str) -> bool {
    let cid = name.strip_prefix(TAP_PREFIX).unwrap_or_default();
    name.len() <= MAX_INTERFACE_NAME && !cid.is_empty() && cid.bytes().all(|b| b.is_ascii_digit())
}

#[derive(Debug, Clone, PartialEq)]
struct CreatedTapGuestAddress {
    tap_interface_name: String,
    guest_vsock_context_id: u32,
    guest_ipv4_address: Ipv4Addr,
    guest_ipv6_address: Option<Ipv6Addr>,
}

impl CreatedTapGuestAddress {
    fn from_checked_tap_name_and_guest_ipv4(
        tap_interface_name: &str,
        guest_ipv4_address: Ipv4Addr,
        guest_ipv6_address: Option<Ipv6Addr>,
    ) -> Result<Self> {
        Ok(Self {
            tap_interface_name: tap_interface_name.to_owned(),
            guest_vsock_context_id: tap_interface_name
                .strip_prefix(TAP_PREFIX)
                .unwrap_or_default()
                .parse()?,
            guest_ipv4_address,
            guest_ipv6_address,
        })
    }

    fn nftables_set_element_text(&self) -> String {
        format!(
            "{{ {} . {} . {} }}",
            self.tap_interface_name,
            guest_mac(self.guest_vsock_context_id),
            self.guest_ipv4_address
        )
    }

    fn nftables_set_element_ipv6_text(&self) -> Option<String> {
        self.guest_ipv6_address.map(|guest_ipv6_address| {
            format!(
                "{{ {} . {} . {} }}",
                self.tap_interface_name,
                guest_mac(self.guest_vsock_context_id),
                guest_ipv6_address
            )
        })
    }

    fn nftables_set_name_and_element_text_pairs(&self) -> Vec<(&'static str, String)> {
        std::iter::once((
            GUEST_TAP_MAC_IPV4_NFTABLES_SET_NAME,
            self.nftables_set_element_text(),
        ))
        .chain(
            self.nftables_set_element_ipv6_text()
                .map(|ipv6_element_text| (GUEST_TAP_IPV6_NFTABLES_SET_NAME, ipv6_element_text)),
        )
        .collect()
    }
}

#[derive(Debug, Clone, PartialEq)]
struct TapAddressRecordWithGuestId {
    tap_guest_address: CreatedTapGuestAddress,
    guest_id: GuestId,
}

impl TapAddressRecordWithGuestId {
    fn from_record_file_text_and_tap_name(text: &str, tap_interface_name: &str) -> Option<Self> {
        let [ipv4_element_text, ipv6_element_text, guest_id_text]: [&str; 3] =
            text.split('\n').collect::<Vec<_>>().try_into().ok()?;
        let guest_ipv6_address = match ipv6_element_text {
            "" => None,
            ipv6_element_text => Some(parse_address_after_last_dot_in_nftables_set_element(
                ipv6_element_text,
            )?),
        };
        let record = Self {
            tap_guest_address: CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                tap_interface_name,
                parse_address_after_last_dot_in_nftables_set_element(ipv4_element_text)?,
                guest_ipv6_address,
            )
            .ok()?,
            guest_id: guest_id_text.parse().ok()?,
        };
        (record.to_record_file_text() == text).then_some(record)
    }

    fn to_record_file_text(&self) -> String {
        format!(
            "{}\n{}\n{}",
            self.tap_guest_address.nftables_set_element_text(),
            self.tap_guest_address
                .nftables_set_element_ipv6_text()
                .unwrap_or_default(),
            self.guest_id
        )
    }
}

#[derive(Debug, PartialEq)]
enum TapRecordFileReadResult {
    ValidThreeLineRecordFile(TapAddressRecordWithGuestId),
    RecordFileDoesNotParseWithSetNameAndElementPairs(Vec<(&'static str, String)>),
}

fn collect_ipv4_or_ipv6_set_elements_from_unparsable_lines_for_tap(
    text: &str,
    tap_interface_name: &str,
) -> Vec<(&'static str, String)> {
    text.lines()
        .filter_map(|line| {
            CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                tap_interface_name,
                parse_address_after_last_dot_in_nftables_set_element(line)
                    .unwrap_or(Ipv4Addr::UNSPECIFIED),
                parse_address_after_last_dot_in_nftables_set_element(line),
            )
            .ok()?
            .nftables_set_name_and_element_text_pairs()
            .into_iter()
            .find(|(_, element_text)| element_text == line)
        })
        .collect()
}

fn parse_address_after_last_dot_in_nftables_set_element<T: FromStr>(
    nftables_set_element_text: &str,
) -> Option<T> {
    nftables_set_element_text
        .strip_suffix(" }")?
        .rsplit_once(" . ")?
        .1
        .parse()
        .ok()
}

fn permanent_ipv6_neighbor_entry_arguments_for_tap(
    tap: &CreatedTapGuestAddress,
    bridge: &str,
) -> Option<Vec<String>> {
    let guest_ipv6_address = tap.guest_ipv6_address?;
    Some(
        [
            "-6",
            "neigh",
            "replace",
            &guest_ipv6_address.to_string(),
            "lladdr",
            &guest_mac(tap.guest_vsock_context_id),
            "dev",
            bridge,
            "nud",
            "permanent",
        ]
        .map(str::to_owned)
        .to_vec(),
    )
}

fn build_ip_command_arguments_to_delete_ipv6_neighbor(
    address: Ipv6Addr,
    bridge: &str,
) -> Vec<String> {
    ["-6", "neigh", "del", &address.to_string(), "dev", bridge]
        .map(str::to_owned)
        .to_vec()
}

fn build_ipv6_permanent_neighbor_show_arguments_for_bridge(bridge: &str) -> Vec<String> {
    ["-6", "neigh", "show", "dev", bridge, "nud", "permanent"]
        .map(str::to_owned)
        .to_vec()
}

fn ip_arguments_to_sync_bridge_neighbour_entries_with_tap_records(
    ipv6_neighbor_show_stdout: &str,
    mut existing_tap_records_with_modification_time: Vec<(SystemTime, CreatedTapGuestAddress)>,
    bridge: &str,
) -> Vec<Vec<String>> {
    existing_tap_records_with_modification_time
        .sort_by_key(|(modified, _)| std::cmp::Reverse(*modified));
    let mut permanent_neighbour_entry_owner_by_guest_ipv6 = BTreeMap::new();
    let mut guest_mac_addresses_taken_by_newer_record = Vec::new();
    for (_, record) in existing_tap_records_with_modification_time {
        let Some(guest_ipv6_address) = record.guest_ipv6_address else {
            continue;
        };
        let mac = guest_mac(record.guest_vsock_context_id);
        if permanent_neighbour_entry_owner_by_guest_ipv6.contains_key(&guest_ipv6_address)
            || guest_mac_addresses_taken_by_newer_record.contains(&mac)
        {
            continue;
        }
        guest_mac_addresses_taken_by_newer_record.push(mac);
        permanent_neighbour_entry_owner_by_guest_ipv6.insert(guest_ipv6_address, record);
    }
    let mut ipv6_addresses_with_permanent_neighbour_entry_matching_mac = Vec::new();
    let mut ip_arguments = Vec::new();
    for line in ipv6_neighbor_show_stdout.lines() {
        let mut fields = line.split_whitespace();
        let Some(Ok(address)) = fields.next().map(str::parse::<Ipv6Addr>) else {
            continue;
        };
        let lladdr = fields
            .skip_while(|field| *field != "lladdr")
            .nth(1)
            .map(str::to_ascii_lowercase);
        let neighbour_entry_matches_record_address_and_mac =
            permanent_neighbour_entry_owner_by_guest_ipv6
                .get(&address)
                .is_some_and(|record| {
                    lladdr.as_deref() == Some(guest_mac(record.guest_vsock_context_id).as_str())
                });
        if neighbour_entry_matches_record_address_and_mac {
            ipv6_addresses_with_permanent_neighbour_entry_matching_mac.push(address);
        } else {
            ip_arguments.push(build_ip_command_arguments_to_delete_ipv6_neighbor(
                address, bridge,
            ));
        }
    }
    ip_arguments.extend(
        permanent_neighbour_entry_owner_by_guest_ipv6
            .iter()
            .filter(|(address, _)| {
                !ipv6_addresses_with_permanent_neighbour_entry_matching_mac.contains(address)
            })
            .filter_map(|(_, record)| {
                permanent_ipv6_neighbor_entry_arguments_for_tap(record, bridge)
            }),
    );
    ip_arguments
}

fn read_all_tap_record_files_with_modification_times_for_existing_tap_devices(
    host: &impl TapReleaseHostCommands,
    record_directory: &Path,
) -> Result<Vec<(SystemTime, CreatedTapGuestAddress)>> {
    Ok(
        list_tap_record_files_with_modification_time_and_read_result(record_directory)?
            .into_iter()
            .filter(|(tap_interface_name, _, _)| {
                host.host_network_device_exists(tap_interface_name)
            })
            .filter_map(
                |(tap_interface_name, modified, read_result)| match read_result {
                    TapRecordFileReadResult::ValidThreeLineRecordFile(record) => {
                        Some((modified, record.tap_guest_address))
                    }
                    TapRecordFileReadResult::RecordFileDoesNotParseWithSetNameAndElementPairs(_) => {
                        warn!(tap = %tap_interface_name, "skip unreadable tap record. remove the IPv6 neighbour entry for that record");
                        None
                    }
                },
            )
            .collect(),
    )
}

async fn sync_guest_bridge_neighbours_with_tap_records(bridge: &str) -> Result<()> {
    let existing_tap_records_with_modification_time =
        read_all_tap_record_files_with_modification_times_for_existing_tap_devices(
            &RealHostCommand,
            Path::new(GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH),
        )?;
    let ipv6_neighbor_show_stdout = run_program_and_get_stdout(
        "ip",
        &build_ipv6_permanent_neighbor_show_arguments_for_bridge(bridge),
    )
    .await?;
    let mut first_failed_ip_command_error = None;
    for arguments in ip_arguments_to_sync_bridge_neighbour_entries_with_tap_records(
        &ipv6_neighbor_show_stdout,
        existing_tap_records_with_modification_time,
        bridge,
    ) {
        if let Err(e) = run("ip", &arguments).await {
            first_failed_ip_command_error.get_or_insert(e);
        }
    }
    first_failed_ip_command_error.map_or(Ok(()), Err)
}

fn tap_isolation_link_set_arguments(tap_interface_name: &str) -> Vec<String> {
    [
        "link",
        "set",
        "dev",
        tap_interface_name,
        "type",
        "bridge_slave",
        "isolated",
        "on",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn build_nft_set_element_command_arguments(
    command: &str,
    nftables_set_name: &str,
    nftables_set_element_text: &str,
) -> Vec<String> {
    [
        command,
        "element",
        "bridge",
        GUEST_BRIDGE_TABLE_NAME,
        nftables_set_name,
        nftables_set_element_text,
    ]
    .map(str::to_owned)
    .to_vec()
}

fn build_nft_call_arguments_for_tap_set_elements(
    command: &str,
    set_name_and_element_text_pairs: &[(&'static str, String)],
) -> Vec<String> {
    set_name_and_element_text_pairs
        .iter()
        .map(|(set_name, element_text)| {
            build_nft_set_element_command_arguments(command, set_name, element_text)
        })
        .collect::<Vec<_>>()
        .join(&";".to_owned())
}

fn guest_tap_nft_add_element_arguments(tap: &CreatedTapGuestAddress) -> Vec<String> {
    build_nft_call_arguments_for_tap_set_elements(
        "add",
        &tap.nftables_set_name_and_element_text_pairs(),
    )
}

fn nft_list_inet_guest_filter_table_arguments() -> [&'static str; 4] {
    ["list", "table", "inet", GUEST_INET_FILTER_TABLE_NAME]
}

trait TapReleaseHostCommands {
    async fn run_program_and_fail_on_non_zero_exit(
        &self,
        program: &str,
        arguments: &[String],
    ) -> Result<()>;

    fn host_network_device_exists(&self, name: &str) -> bool;
}

struct RealHostCommand;

impl TapReleaseHostCommands for RealHostCommand {
    async fn run_program_and_fail_on_non_zero_exit(
        &self,
        program: &str,
        arguments: &[String],
    ) -> Result<()> {
        run(program, arguments).await
    }

    fn host_network_device_exists(&self, name: &str) -> bool {
        Path::new(SYSFS_NET).join(name).exists()
    }
}

async fn delete_nftables_set_elements_ensuring_absent(
    host: &impl TapReleaseHostCommands,
    set_name_and_element_text_pairs: &[(&'static str, String)],
) -> Result<()> {
    if set_name_and_element_text_pairs.is_empty()
        || host
            .run_program_and_fail_on_non_zero_exit(
                "nft",
                &build_nft_call_arguments_for_tap_set_elements(
                    "delete",
                    set_name_and_element_text_pairs,
                ),
            )
            .await
            .is_ok()
    {
        return Ok(());
    }
    for (set_name_to_delete, set_element_text_to_delete) in set_name_and_element_text_pairs {
        let Err(nft_delete_element_command_error) = host
            .run_program_and_fail_on_non_zero_exit(
                "nft",
                &build_nft_set_element_command_arguments(
                    "delete",
                    set_name_to_delete,
                    set_element_text_to_delete,
                ),
            )
            .await
        else {
            continue;
        };
        if host
            .run_program_and_fail_on_non_zero_exit(
                "nft",
                &build_nft_set_element_command_arguments(
                    "get",
                    set_name_to_delete,
                    set_element_text_to_delete,
                ),
            )
            .await
            .is_ok()
        {
            return Err(nft_delete_element_command_error);
        }
    }
    Ok(())
}

async fn delete_tap_record_nft_elements_file_and_device(
    host: &impl TapReleaseHostCommands,
    record_directory: &Path,
    tap_interface_name: &str,
    set_name_and_element_text_pairs: &[(&'static str, String)],
) -> Result<()> {
    ensure!(
        is_tap_name(tap_interface_name),
        "{tap_interface_name:?} is not a guest tap name"
    );
    let delete_tap_nftables_set_elements_result =
        delete_nftables_set_elements_ensuring_absent(host, set_name_and_element_text_pairs).await;
    let tap_network_device_delete_result = if host.host_network_device_exists(tap_interface_name) {
        host.run_program_and_fail_on_non_zero_exit(
            "ip",
            &["link", "del", "dev", tap_interface_name].map(str::to_owned),
        )
        .await
    } else {
        Ok(())
    };
    delete_tap_nftables_set_elements_result.and(tap_network_device_delete_result)?;
    let path = record_directory.join(tap_interface_name);
    match std::fs::remove_file(&path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(anyhow::Error::from(e).context(
            format!("Delete the tap record {path}", path = path.display()),
        )),
        _ => Ok(()),
    }
}

async fn delete_leftover_tap_record_nft_elements_file_and_device_for_tap_name(
    host: &impl TapReleaseHostCommands,
    record_directory: &Path,
    tap_interface_name: &str,
) -> Result<()> {
    let set_name_and_element_text_pairs_of_earlier_guest =
        match read_tap_record_from_directory(record_directory, tap_interface_name)? {
            Some(TapRecordFileReadResult::ValidThreeLineRecordFile(record)) => record
                .tap_guest_address
                .nftables_set_name_and_element_text_pairs(),
            Some(TapRecordFileReadResult::RecordFileDoesNotParseWithSetNameAndElementPairs(
                set_name_and_element_text_pairs,
            )) => set_name_and_element_text_pairs,
            None => Vec::new(),
        };
    delete_tap_record_nft_elements_file_and_device(
        host,
        record_directory,
        tap_interface_name,
        &set_name_and_element_text_pairs_of_earlier_guest,
    )
    .await
}

async fn write_tap_record_then_create_tap_device(
    host: &impl TapReleaseHostCommands,
    record_directory: &Path,
    record: &TapAddressRecordWithGuestId,
    bridge: &str,
    tap_device_owner_user_id_and_group_id: (&str, &str),
    sync_permanent_ipv6_neighbour_entries_after_adding_tap: bool,
) -> Result<()> {
    let tap = &record.tap_guest_address;
    let name = tap.tap_interface_name.as_str();
    let (user, group) = tap_device_owner_user_id_and_group_id;
    write_element_record_atomically(record_directory, record)?;
    let tap_preparation_after_record_write_result = async {
        host.run_program_and_fail_on_non_zero_exit(
            "ip",
            &[
                "tuntap", "add", "dev", name, "mode", "tap", "user", user, "group", group,
            ]
            .map(str::to_owned),
        )
        .await?;
        host.run_program_and_fail_on_non_zero_exit(
            "ip",
            &["link", "set", name, "master", bridge, "up"].map(str::to_owned),
        )
        .await?;
        host.run_program_and_fail_on_non_zero_exit("ip", &tap_isolation_link_set_arguments(name))
            .await?;
        host.run_program_and_fail_on_non_zero_exit(
            "nft",
            &guest_tap_nft_add_element_arguments(tap),
        )
        .await?;
        if sync_permanent_ipv6_neighbour_entries_after_adding_tap {
            sync_guest_bridge_neighbours_with_tap_records(bridge).await?;
        }
        anyhow::Ok(())
    }
    .await;
    let Err(e) = tap_preparation_after_record_write_result else {
        return Ok(());
    };
    if let Err(release_error) = delete_tap_record_nft_elements_file_and_device(
        host,
        record_directory,
        name,
        &tap.nftables_set_name_and_element_text_pairs(),
    )
    .await
    {
        warn!(tap = %name, error = %format!("{release_error:#}"), "Preparation of the tap failed. Release of the tap failed after that. The record is still present, so later starts or stops of any guest on the host retry the release.");
    }
    if sync_permanent_ipv6_neighbour_entries_after_adding_tap
        && let Err(sync_error) = sync_guest_bridge_neighbours_with_tap_records(bridge).await
    {
        warn!(bridge = %bridge, error = %format!("{sync_error:#}"), "Failed to match the permanent IPv6 neighbour entries on the guest bridge to the tap records after a failed tap preparation. The next start or stop of a guest retries.");
    }
    Err(e)
}

fn is_guest_systemd_runtime_directory_missing(
    guest_runtime_base_directory_path: &Path,
    guest_id: &GuestId,
) -> bool {
    std::fs::symlink_metadata(guest_runtime_base_directory_path.join(guest_id.to_string()))
        .is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn select_tap_record_files_to_release_for_stopping_guest_missing_runtime_or_unparsed(
    tap_record_files: Vec<(String, SystemTime, TapRecordFileReadResult)>,
    guest_runtime_base_directory_path: &Path,
    stopped_guest_id: Option<&GuestId>,
) -> Vec<(String, Vec<(&'static str, String)>)> {
    tap_record_files
        .into_iter()
        .filter_map(
            |(tap_interface_name, _, read_result)| match read_result {
                TapRecordFileReadResult::ValidThreeLineRecordFile(record) => {
                    (stopped_guest_id == Some(&record.guest_id)
                        || is_guest_systemd_runtime_directory_missing(
                            guest_runtime_base_directory_path,
                            &record.guest_id,
                        ))
                    .then(|| {
                        (
                            tap_interface_name,
                            record
                                .tap_guest_address
                                .nftables_set_name_and_element_text_pairs(),
                        )
                    })
                }
                TapRecordFileReadResult::RecordFileDoesNotParseWithSetNameAndElementPairs(
                    set_name_and_element_text_pairs,
                ) => {
                    warn!(tap = %tap_interface_name, "Cannot parse tap record. No running guests match this tap record. Remove the tap, the set elements the record still names and the record.");
                    Some((tap_interface_name, set_name_and_element_text_pairs))
                }
            },
        )
        .collect()
}

async fn release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
    host: &impl TapReleaseHostCommands,
    record_directory: &Path,
    guest_runtime_base_directory_path: &Path,
    stopped_guest_id: Option<&GuestId>,
) -> Result<()> {
    let mut first_error = None;
    for (tap_interface_name, set_name_and_element_text_pairs) in
        select_tap_record_files_to_release_for_stopping_guest_missing_runtime_or_unparsed(
            list_tap_record_files_with_modification_time_and_read_result(record_directory)?,
            guest_runtime_base_directory_path,
            stopped_guest_id,
        )
    {
        if let Err(e) = delete_tap_record_nft_elements_file_and_device(
            host,
            record_directory,
            &tap_interface_name,
            &set_name_and_element_text_pairs,
        )
        .await
        {
            first_error.get_or_insert(e);
        }
    }
    first_error.map_or(Ok(()), Err)
}

pub fn release_guest_taps_and_sync_ipv6_neighbours_after_supervisor_stop(
    guest_id: &GuestId,
    config: &SupervisorConfig,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    runtime.block_on(async {
        let locked_tap_record_directory_file = lock_tap_record_directory().await?;
        let release_result =
            release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
                &RealHostCommand,
                Path::new(GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH),
                Path::new(GUEST_RUN_DIRECTORY_BASE_PATH),
                Some(guest_id),
            )
            .await;
        let sync_result = match config.guest_bridge_ipv6_subnet {
            Some(_) => sync_guest_bridge_neighbours_with_tap_records(&config.bridge).await,
            None => Ok(()),
        };
        drop(locked_tap_record_directory_file);
        release_result.and(sync_result)
    })
}

fn tap_record_file_path(record_directory: &Path, tap_interface_name: &str) -> Result<PathBuf> {
    ensure!(
        is_tap_name(tap_interface_name),
        "{tap_interface_name:?} is not a guest tap name"
    );
    Ok(record_directory.join(tap_interface_name))
}

fn create_record_directory_if_missing_and_check_without_following_symlinks_for_root_owner_and_no_group_or_other_write()
-> Result<std::fs::File> {
    let path = Path::new(GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH);
    match std::fs::DirBuilder::new().mode(0o700).create(path) {
        Err(e) if e.kind() != std::io::ErrorKind::AlreadyExists => {
            return Err(e).with_context(|| format!("create directory {}", path.display()));
        }
        _ => {}
    }
    let open_without_following_symlinks_error_text =
        || format!("open {} without following symlinks", path.display());
    let opened_tap_record_directory_file_descriptor = rustix::fs::open(
        path,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .with_context(open_without_following_symlinks_error_text)?;
    let record_dir_stat = rustix::fs::fstat(&opened_tap_record_directory_file_descriptor)
        .with_context(open_without_following_symlinks_error_text)?;
    check_directory_has_required_owner_and_no_group_or_other_write(
        path,
        record_dir_stat.st_uid,
        record_dir_stat.st_mode,
        REQUIRED_ANCESTOR_OWNER_USER_ID,
    )?;
    Ok(std::fs::File::from(
        opened_tap_record_directory_file_descriptor,
    ))
}

async fn lock_tap_record_directory() -> Result<std::fs::File> {
    wait_for_exclusive_directory_lock_with_timeout(
        create_record_directory_if_missing_and_check_without_following_symlinks_for_root_owner_and_no_group_or_other_write()?,
        TAP_RECORD_DIRECTORY_LOCK_WAIT_DURATION,
    )
    .await
    .with_context(|| format!("Lock {GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH}"))
}

async fn wait_for_exclusive_directory_lock_with_timeout(
    directory: std::fs::File,
    wait_limit: Duration,
) -> Result<std::fs::File> {
    let lock_wait_deadline = tokio::time::Instant::now() + wait_limit;
    loop {
        match directory.try_lock() {
            Ok(()) => return Ok(directory),
            Err(std::fs::TryLockError::WouldBlock)
                if tokio::time::Instant::now() < lock_wait_deadline =>
            {
                tokio::time::sleep(TAP_RECORD_DIRECTORY_LOCK_RETRY_SLEEP_DURATION).await;
            }
            Err(std::fs::TryLockError::WouldBlock) => {
                bail!(
                    "could not acquire the lock of the tap record directory within {wait_limit:?}"
                )
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e.into()),
        }
    }
}

fn read_tap_record_from_directory(
    record_directory: &Path,
    tap_interface_name: &str,
) -> Result<Option<TapRecordFileReadResult>> {
    ensure!(
        is_tap_name(tap_interface_name),
        "{tap_interface_name:?} is not a guest tap name"
    );
    let path = record_directory.join(tap_interface_name);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e).with_context(|| format!("Read record {}", path.display())),
    };
    let text = String::from_utf8_lossy(&bytes);
    Ok(Some(
        match TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
            &text,
            tap_interface_name,
        ) {
            Some(record) => TapRecordFileReadResult::ValidThreeLineRecordFile(record),
            None => TapRecordFileReadResult::RecordFileDoesNotParseWithSetNameAndElementPairs(
                collect_ipv4_or_ipv6_set_elements_from_unparsable_lines_for_tap(
                    &text,
                    tap_interface_name,
                ),
            ),
        },
    ))
}

fn list_tap_record_files_with_modification_time_and_read_result(
    record_directory: &Path,
) -> Result<Vec<(String, SystemTime, TapRecordFileReadResult)>> {
    let entries = match std::fs::read_dir(record_directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => {
            return Err(e).with_context(|| format!("Read record {}", record_directory.display()));
        }
    };
    let mut tap_record_files = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(e) => {
                warn!(path = %record_directory.display(), error = %e, "Skipped one entry of the tap record. Reading that entry failed.");
                continue;
            }
        };
        let Some(tap_interface_name) = entry
            .file_name()
            .to_str()
            .filter(|name| is_tap_name(name))
            .map(str::to_owned)
        else {
            continue;
        };
        let tap_record_with_modified_time_read_result = entry
            .metadata()
            .and_then(|metadata| metadata.modified())
            .map_err(anyhow::Error::from)
            .and_then(|modified| {
                Ok((
                    modified,
                    read_tap_record_from_directory(record_directory, &tap_interface_name)?,
                ))
            });
        match tap_record_with_modified_time_read_result {
            Ok((modified, Some(read_result))) => {
                tap_record_files.push((tap_interface_name, modified, read_result));
            }
            Ok((_, None)) => {}
            Err(e) => {
                warn!(path = %entry.path().display(), error = %format!("{e:#}"), "Skipped one entry of the tap record. Reading that entry failed.");
            }
        }
    }
    tap_record_files.sort_by(|(left, _, _), (right, _, _)| left.cmp(right));
    Ok(tap_record_files)
}

fn write_element_record_atomically(
    record_directory: &Path,
    record: &TapAddressRecordWithGuestId,
) -> Result<()> {
    let path = tap_record_file_path(
        record_directory,
        &record.tap_guest_address.tap_interface_name,
    )?;
    let temporary_record_path = path.with_extension("tmp");
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary_record_path)
        .and_then(|mut file| file.write_all(record.to_record_file_text().as_bytes()))
        .and_then(|()| std::fs::rename(&temporary_record_path, &path))
        .with_context(|| format!("Write a record to {}", path.display()))
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

fn usb_device_is_bound_to_usbip_host_driver(sysfs_usb_devices: &Path, bus_id: &str) -> bool {
    std::fs::read_link(sysfs_usb_devices.join(bus_id).join("driver"))
        .is_ok_and(|driver| driver.file_name().is_some_and(|name| name == "usbip-host"))
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
    tag: &str,
    shared: &OwnedFd,
    read_only: bool,
    ids: &[String],
) -> Result<Child> {
    let _ = std::fs::remove_file(socket);
    // The child process can start inside the checked directory through its own `/proc/self/fd` link. virtiofsd can then serve that directory as `--shared-dir=.` even after a new `/proc` mount by the sandbox. No descriptor copy can remain after exec.
    let mut command = Command::new(bin);
    command
        .current_dir(format!("/proc/self/fd/{}", shared.as_raw_fd()))
        .arg(format!("--socket-path={}", socket.display()))
        .arg("--shared-dir=.")
        .arg("--sandbox=namespace")
        .arg("--cache=auto")
        .args(ids)
        .kill_on_drop(true);
    if read_only {
        command.arg("--readonly");
    }
    command
        .spawn()
        .with_context(|| format!("start {bin} for mount {tag}"))
}

fn bind(socket: &Path) -> Result<UnixListener> {
    let _ = std::fs::remove_file(socket);
    UnixListener::bind(socket).with_context(|| format!("listen on {}", socket.display()))
}

fn share(socket: &Path, group: u32) -> Result<()> {
    std::os::unix::fs::chown(socket, None, Some(group))
        .with_context(|| format!("hand {} to group {group}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(SHARED_SOCKET_MODE))
        .with_context(|| format!("open {} to group {group}", socket.display()))
}

pub fn guest_supervisor_directory_path(guest_id: &GuestId) -> PathBuf {
    Path::new(GUEST_RUN_DIRECTORY_BASE_PATH).join(guest_id.to_string())
}

fn supervisor_request_error_to_reply(error: anyhow::Error) -> SupervisorReply {
    let message = format!("{error:#}");
    match error
        .chain()
        .any(|cause| cause.is::<GuestSupervisorHostConfigurationError>())
    {
        true => SupervisorReply::RejectedByConfiguration { message },
        false => SupervisorReply::Failed { message },
    }
}

fn open_directory_path_from_trusted_start_without_symlinks_requiring_owned_ancestors(
    trusted_walk_start_directory_path: &Path,
    path: &Path,
    required_ancestor_owner_user_id: u32,
) -> Result<OwnedFd> {
    let directory_flags = OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let file_names_after_start_directory = path
        .strip_prefix(trusted_walk_start_directory_path)
        .ok()
        .filter(|_| path.is_absolute())
        .and_then(|relative_path| {
            relative_path
                .components()
                .map(|component| match component {
                    std::path::Component::Normal(file_name) => Some(file_name),
                    _ => None,
                })
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| {
            GuestSupervisorHostConfigurationError(format!(
                "Rejected: {} is not an absolute path without `..` components.",
                path.display()
            ))
        })?;
    if file_names_after_start_directory.is_empty() {
        let trusted_directory_display = trusted_walk_start_directory_path.display();
        return Err(GuestSupervisorHostConfigurationError(format!(
            "Rejected: {path} does not point to a directory under {start}. Configure a directory under {start}.",
            path = path.display(),
            start = trusted_directory_display
        ))
        .into());
    }
    let mut current_directory_path = trusted_walk_start_directory_path.to_path_buf();
    let mut current_directory_descriptor = rustix::fs::open(
        trusted_walk_start_directory_path,
        directory_flags,
        Mode::empty(),
    )
    .with_context(|| {
        format!(
            "open {} without following symlinks",
            trusted_walk_start_directory_path.display()
        )
    })?;
    for file_name in file_names_after_start_directory {
        let current_ancestor_directory_owner_and_mode =
            rustix::fs::fstat(&current_directory_descriptor).with_context(|| {
                format!("Read the owner of {}", current_directory_path.display())
            })?;
        check_directory_has_required_owner_and_no_group_or_other_write(
            &current_directory_path,
            current_ancestor_directory_owner_and_mode.st_uid,
            current_ancestor_directory_owner_and_mode.st_mode,
            required_ancestor_owner_user_id,
        )?;
        current_directory_path.push(file_name);
        current_directory_descriptor = match rustix::fs::openat(
            &current_directory_descriptor,
            file_name,
            directory_flags,
            Mode::empty(),
        ) {
            Ok(descriptor) => descriptor,
            Err(_)
                if rustix::fs::statat(
                    &current_directory_descriptor,
                    file_name,
                    rustix::fs::AtFlags::SYMLINK_NOFOLLOW,
                )
                .is_ok_and(|stat| {
                    rustix::fs::FileType::from_raw_mode(stat.st_mode)
                        == rustix::fs::FileType::Symlink
                }) =>
            {
                return Err(GuestSupervisorHostConfigurationError(format!(
                    "Rejected: {} is a symlink. Set the resolved path of the host directory instead.",
                    current_directory_path.display()
                ))
                .into());
            }
            Err(e) => {
                return Err(anyhow::Error::from(e).context(format!(
                    "open {} without following symlinks",
                    current_directory_path.display()
                )));
            }
        };
    }
    Ok(current_directory_descriptor)
}

fn check_directory_has_required_owner_and_no_group_or_other_write(
    directory: &Path,
    owner_user_id: u32,
    mode: u32,
    required_owner_user_id: u32,
) -> Result<(), GuestSupervisorHostConfigurationError> {
    let directory = directory.display();
    if owner_user_id != required_owner_user_id {
        return Err(GuestSupervisorHostConfigurationError(format!(
            "Refuse: user {owner_user_id} is the owner of {directory}, not user {required_owner_user_id}."
        )));
    }
    if mode & 0o022 != 0 {
        return Err(GuestSupervisorHostConfigurationError(format!(
            "Denied: group or other users can write to {directory}."
        )));
    }
    Ok(())
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

async fn run(program: &str, args: &[impl AsRef<str>]) -> Result<()> {
    run_program_and_get_stdout(program, args).await.map(drop)
}

async fn run_program_and_get_stdout(program: &str, args: &[impl AsRef<str>]) -> Result<String> {
    run_program_and_get_stdout_with_time_limit(program, args, HOST_COMMAND_TIMEOUT_DURATION).await
}

async fn run_program_and_get_stdout_with_time_limit(
    program: &str,
    args: &[impl AsRef<str>],
    limit: Duration,
) -> Result<String> {
    let args: Vec<&str> = args.iter().map(AsRef::as_ref).collect();
    let program_and_arguments_text_for_error_messages = format!("{program} {}", args.join(" "));
    let mut host_command_child = Command::new(program)
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("run {program}"))?;
    let (Some(mut child_stdout_pipe), Some(mut child_stderr_pipe)) = (
        host_command_child.stdout.take(),
        host_command_child.stderr.take(),
    ) else {
        bail!("run {program}");
    };
    let (mut child_stdout_bytes, mut child_stderr_bytes) = (Vec::new(), Vec::new());
    let child_wait_within_time_limit_result = tokio::time::timeout(limit, async {
        tokio::try_join!(
            child_stdout_pipe.read_to_end(&mut child_stdout_bytes),
            child_stderr_pipe.read_to_end(&mut child_stderr_bytes)
        )?;
        host_command_child.wait().await
    })
    .await;
    let Ok(host_command_exit_status_result) = child_wait_within_time_limit_result else {
        host_command_child
            .kill()
            .await
            .with_context(|| format!("kill {program}"))?;
        bail!(
            "{program_and_arguments_text_for_error_messages} did not exit within {limit:?}. The supervisor killed the process."
        );
    };
    if !host_command_exit_status_result
        .with_context(|| format!("run {program}"))?
        .success()
    {
        bail!(
            "{program_and_arguments_text_for_error_messages} failed: {}",
            String::from_utf8_lossy(&child_stderr_bytes).trim()
        );
    }
    String::from_utf8(child_stdout_bytes).with_context(|| format!("read output of {program}"))
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{DeviceConfig, MountConfig};
    use crate::guest::WORK_MOUNT_TAG;

    #[tokio::test]
    async fn directory_lock_second_attempt_fails_while_first_handle_holds_lock_and_succeeds_after_drop()
     {
        let temporary_tap_record_directory = tempfile::tempdir().unwrap();
        let open_temp_directory_handle =
            || std::fs::File::open(temporary_tap_record_directory.path()).unwrap();
        let first_temporary_directory_lock_handle = wait_for_exclusive_directory_lock_with_timeout(
            open_temp_directory_handle(),
            Duration::ZERO,
        )
        .await
        .unwrap();
        let second_lock_attempt_error = wait_for_exclusive_directory_lock_with_timeout(
            open_temp_directory_handle(),
            Duration::ZERO,
        )
        .await
        .unwrap_err();
        assert!(
            second_lock_attempt_error
                .to_string()
                .contains("could not acquire the lock")
        );
        drop(first_temporary_directory_lock_handle);
        wait_for_exclusive_directory_lock_with_timeout(
            open_temp_directory_handle(),
            Duration::ZERO,
        )
        .await
        .unwrap();
    }

    #[test]
    fn test_supervisor_directory_check_accepts_root_owner_mode_0750_and_rejects_non_root_owner_or_group_or_other_write_bit()
     {
        assert!(
            check_directory_has_required_owner_and_no_group_or_other_write(
                Path::new(GUEST_RUN_DIRECTORY_BASE_PATH),
                0,
                0o40750,
                REQUIRED_ANCESTOR_OWNER_USER_ID
            )
            .is_ok()
        );
        assert!(
            check_directory_has_required_owner_and_no_group_or_other_write(
                Path::new(GUEST_RUN_DIRECTORY_BASE_PATH),
                1000,
                0o40750,
                REQUIRED_ANCESTOR_OWNER_USER_ID
            )
            .is_err()
        );
        assert!(
            check_directory_has_required_owner_and_no_group_or_other_write(
                Path::new(GUEST_RUN_DIRECTORY_BASE_PATH),
                0,
                0o40770,
                REQUIRED_ANCESTOR_OWNER_USER_ID
            )
            .is_err()
        );
        assert!(
            check_directory_has_required_owner_and_no_group_or_other_write(
                Path::new(GUEST_RUN_DIRECTORY_BASE_PATH),
                0,
                0o40752,
                REQUIRED_ANCESTOR_OWNER_USER_ID
            )
            .is_err()
        );
    }

    fn usb_device(root: &Path, name: &str, vendor: &str, product: Option<&str>) {
        let dir = root.join(name);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("idVendor"), format!("{vendor}\n")).unwrap();
        if let Some(product) = product {
            std::fs::write(dir.join("idProduct"), format!("{product}\n")).unwrap();
        }
    }

    #[test]
    fn usb_device_is_bound_only_when_driver_symlink_points_to_usbip_host() {
        let root = tempfile::tempdir().unwrap();
        usb_device(root.path(), "1-2", "1d6b", Some("0002"));
        usb_device(root.path(), "1-3", "046d", Some("c52b"));
        std::os::unix::fs::symlink(
            "../../../bus/usb/drivers/usbip-host",
            root.path().join("1-2").join("driver"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            "../../../bus/usb/drivers/usb",
            root.path().join("1-3").join("driver"),
        )
        .unwrap();

        assert!(usb_device_is_bound_to_usbip_host_driver(root.path(), "1-2"));
        assert!(!usb_device_is_bound_to_usbip_host_driver(
            root.path(),
            "1-3"
        ));
        assert!(!usb_device_is_bound_to_usbip_host_driver(
            root.path(),
            "1-9"
        ));
    }

    #[test]
    fn prepared_virtiofs_socket_is_readable_and_writable_by_guest_directory_group() {
        use std::os::unix::fs::MetadataExt;
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("scratch.sock");
        std::fs::File::create(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let group = std::fs::metadata(temp.path()).unwrap().gid();

        share(&socket, group).unwrap();

        let socket = std::fs::metadata(&socket).unwrap();
        assert_eq!(socket.mode() & 0o777, SHARED_SOCKET_MODE);
        assert_eq!(socket.gid(), group);
    }

    #[tokio::test]
    async fn spawn_virtiofsd_creates_vhost_user_socket_while_child_is_running_for_directory_descriptor()
     {
        #[expect(
            clippy::disallowed_methods,
            reason = "PATH is a host variable. The test can find the virtiofsd binary through PATH."
        )]
        let virtiofsd_binary_path = std::env::var_os("PATH").and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("virtiofsd"))
                .find(|candidate| candidate.is_file())
        });
        let can_create_user_namespace =
            !std::fs::read_to_string("/proc/sys/user/max_user_namespaces")
                .is_ok_and(|limit| limit.trim() == "0");
        let Some(virtiofsd_binary_path) =
            virtiofsd_binary_path.filter(|_| can_create_user_namespace)
        else {
            eprintln!(
                "Skip: no virtiofsd in the host PATH, or the host does not allow user namespaces."
            );
            return;
        };
        let temp = tempfile::tempdir().unwrap();
        let shared = temp.path().join("shared");
        std::fs::create_dir(&shared).unwrap();
        let virtiofsd_shared_directory_descriptor = rustix::fs::open(
            &shared,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .unwrap();
        let socket = temp.path().join("scratch.sock");
        let ids = owner_translation(
            rustix::process::getuid().as_raw(),
            rustix::process::getgid().as_raw(),
        );

        let mut child = spawn_virtiofsd(
            virtiofsd_binary_path.to_str().unwrap(),
            &socket,
            "scratch",
            &virtiofsd_shared_directory_descriptor,
            false,
            &ids,
        )
        .unwrap();

        tokio::select! {
            appeared = wait_for_socket(&socket) => appeared.unwrap(),
            exited = child.wait() => panic!("virtiofsd exited before the socket appeared: {exited:?}"),
        }
        assert!(child.try_wait().unwrap().is_none());
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
        let mount = |name: &str, read_only, per_job| {
            MountConfig {
            name: name.to_owned(),
            projects: gradient_wire::host_resource_project_access::WorkerResourceProjectAccess::EveryProject,
            path: PathBuf::from(format!("/srv/{name}")),
            read_only,
            per_job,
        }
        };
        SupervisorConfig {
            virtiofsd_bin: "virtiofsd".to_owned(),
            devices: vec![
                DeviceConfig {
                    name: "serial".to_owned(),
                    vfio: Vec::new(),
                    projects: gradient_wire::host_resource_project_access::WorkerResourceProjectAccess::EveryProject,
                    dev: Some(PathBuf::from("/dev/ttyUSB0")),
                    usb: None,
                },
                DeviceConfig {
                    name: "camera".to_owned(),
                    vfio: Vec::new(),
                    projects: gradient_wire::host_resource_project_access::WorkerResourceProjectAccess::EveryProject,
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
            guest_bridge_ipv4_subnet: Some(TEST_GUEST_SUBNET.parse().unwrap()),
            guest_bridge_ipv6_subnet: None,
        }
    }

    const TEST_GUEST_SUBNET: &str = "10.233.0.0/24";

    fn build_prepare_tap_request(
        name: &str,
        bridge: &str,
        guest_ipv4_address: Option<Ipv4Addr>,
    ) -> SupervisorRequest {
        SupervisorRequest::PrepareTap {
            name: name.to_owned(),
            bridge: bridge.to_owned(),
            guest_ipv4_address,
        }
    }

    fn prepare_mount_request(tag: &str, read_only: bool) -> SupervisorRequest {
        SupervisorRequest::PrepareMount {
            tag: tag.to_owned(),
            read_only,
        }
    }

    fn allowed(request: SupervisorRequest) -> bool {
        let bound = ["1-2".to_owned()];
        check_request(&worker_config(), &bound, &request).is_ok()
    }

    #[test]
    fn supervisor_accepts_only_tags_of_configured_mounts_and_refuses_writable_requests_for_read_only_mounts()
     {
        assert!(allowed(prepare_mount_request("cache", false)));
        assert!(allowed(prepare_mount_request("datasets", true)));
        assert!(allowed(prepare_mount_request("scratch", false)));

        assert!(!allowed(prepare_mount_request("datasets", false)));
        assert!(!allowed(prepare_mount_request("root", false)));
        assert!(!allowed(prepare_mount_request(WORK_MOUNT_TAG, false)));
        assert!(!allowed(prepare_mount_request("../etc", true)));
    }

    fn open_test_scratch_mount_with_temp_trusted_start_and_test_user_owner(
        config: &SupervisorConfig,
        guest_id: &GuestId,
        walk_trusted_start_temp_directory: &Path,
    ) -> Result<OwnedFd> {
        use std::os::unix::fs::MetadataExt;
        let worker_owned_guest_directory_temp_dir = tempfile::tempdir().unwrap();
        let worker_guest_directory =
            OpenedGuestDirectory::open_guest_directory_without_symlink_and_reject_root_owner(
                &std::fs::canonicalize(worker_owned_guest_directory_temp_dir.path()).unwrap(),
            )
            .unwrap();
        let required_ancestor_owner_user_id = std::fs::metadata(walk_trusted_start_temp_directory)
            .unwrap()
            .uid();
        open_host_mount_directory_and_create_job_directory_owned_by_worker(
            config,
            &worker_guest_directory,
            guest_id,
            "scratch",
            walk_trusted_start_temp_directory,
            required_ancestor_owner_user_id,
        )
    }

    fn create_trusted_path_start_temp_dir() -> tempfile::TempDir {
        let walk_trusted_start_temp_directory = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            walk_trusted_start_temp_directory.path(),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        walk_trusted_start_temp_directory
    }

    fn create_directory_with_mode(path: &Path, mode: u32) {
        std::fs::create_dir(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn error_chain_has_permanent_guest_supervisor_host_configuration_error(
        mount_opening_error: &anyhow::Error,
    ) -> bool {
        mount_opening_error
            .chain()
            .any(|cause| cause.is::<GuestSupervisorHostConfigurationError>())
    }

    #[test]
    fn test_refuses_worker_symlink_at_job_mount_path_and_creates_nothing_in_outside_directory() {
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        let outside_mount_symlink_target_temp_dir = tempfile::tempdir().unwrap();
        let guest_id = GuestId::new();
        let mut config = worker_config();
        config.mounts[2].path = walk_trusted_start_temp_directory.path().join("scratch");
        create_directory_with_mode(&config.mounts[2].path, 0o770);
        std::os::unix::fs::symlink(
            outside_mount_symlink_target_temp_dir.path(),
            config.mounts[2].path.join(guest_id.to_string()),
        )
        .unwrap();

        assert!(
            open_test_scratch_mount_with_temp_trusted_start_and_test_user_owner(
                &config,
                &guest_id,
                walk_trusted_start_temp_directory.path(),
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read_dir(outside_mount_symlink_target_temp_dir.path())
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn reject_mount_path_symlinked_to_outside_directory_as_permanent_error_without_creating_file_or_directory()
     {
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        let outside_mount_symlink_target_temp_dir = tempfile::tempdir().unwrap();
        let guest_id = GuestId::new();
        let mut config = worker_config();
        config.mounts[2].path = walk_trusted_start_temp_directory.path().join("scratch");
        std::os::unix::fs::symlink(
            outside_mount_symlink_target_temp_dir.path(),
            &config.mounts[2].path,
        )
        .unwrap();

        let mount_opening_error =
            open_test_scratch_mount_with_temp_trusted_start_and_test_user_owner(
                &config,
                &guest_id,
                walk_trusted_start_temp_directory.path(),
            )
            .unwrap_err();
        assert!(
            error_chain_has_permanent_guest_supervisor_host_configuration_error(
                &mount_opening_error
            )
        );
        assert_eq!(
            std::fs::read_dir(outside_mount_symlink_target_temp_dir.path())
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn test_mount_path_with_ancestor_directory_writable_by_others_is_refused_as_permanent() {
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        let world_writable_ancestor_directory =
            walk_trusted_start_temp_directory.path().join("shared");
        create_directory_with_mode(&world_writable_ancestor_directory, 0o757);
        let guest_id = GuestId::new();
        let mut config = worker_config();
        config.mounts[2].path = world_writable_ancestor_directory.join("scratch");
        create_directory_with_mode(&config.mounts[2].path, 0o770);

        let mount_opening_error =
            open_test_scratch_mount_with_temp_trusted_start_and_test_user_owner(
                &config,
                &guest_id,
                walk_trusted_start_temp_directory.path(),
            )
            .unwrap_err();
        assert!(
            error_chain_has_permanent_guest_supervisor_host_configuration_error(
                &mount_opening_error
            )
        );
        assert!(
            !config.mounts[2]
                .host_directory_for_guest(&guest_id)
                .exists()
        );
    }

    #[test]
    fn mount_path_with_symlink_ancestor_is_refused_and_error_tells_admin_to_configure_resolved_path()
     {
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        let resolved_symlink_ancestor_target_directory =
            walk_trusted_start_temp_directory.path().join("real");
        create_directory_with_mode(&resolved_symlink_ancestor_target_directory, 0o755);
        create_directory_with_mode(
            &resolved_symlink_ancestor_target_directory.join("scratch"),
            0o770,
        );
        let ancestor_symlink_path = walk_trusted_start_temp_directory.path().join("link");
        std::os::unix::fs::symlink(
            &resolved_symlink_ancestor_target_directory,
            &ancestor_symlink_path,
        )
        .unwrap();
        let guest_id = GuestId::new();
        let mut config = worker_config();
        config.mounts[2].path = ancestor_symlink_path.join("scratch");

        let mount_opening_error =
            open_test_scratch_mount_with_temp_trusted_start_and_test_user_owner(
                &config,
                &guest_id,
                walk_trusted_start_temp_directory.path(),
            )
            .unwrap_err();
        assert!(
            error_chain_has_permanent_guest_supervisor_host_configuration_error(
                &mount_opening_error
            )
        );
        assert!(
            format!("{mount_opening_error:#}")
                .contains("Set the resolved path of the host directory instead.")
        );
        assert!(
            !resolved_symlink_ancestor_target_directory
                .join("scratch")
                .join(guest_id.to_string())
                .exists()
        );
    }

    fn assert_walk_fails_permanently_with_temp_trusted_start_dir_and_temp_dir_owner(
        walk_trusted_start_temp_directory: &Path,
        path: &Path,
        required_ancestor_owner_user_id: u32,
    ) {
        let mount_opening_error =
            open_directory_path_from_trusted_start_without_symlinks_requiring_owned_ancestors(
                walk_trusted_start_temp_directory,
                path,
                required_ancestor_owner_user_id,
            )
            .unwrap_err();
        assert!(
            error_chain_has_permanent_guest_supervisor_host_configuration_error(
                &mount_opening_error
            )
        );
    }

    #[test]
    fn host_directory_walk_refuses_relative_configured_mount_path_with_permanent_configuration_error()
     {
        use std::os::unix::fs::MetadataExt;
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        create_directory_with_mode(
            &walk_trusted_start_temp_directory.path().join("scratch"),
            0o770,
        );

        assert_walk_fails_permanently_with_temp_trusted_start_dir_and_temp_dir_owner(
            walk_trusted_start_temp_directory.path(),
            Path::new("scratch"),
            std::fs::metadata(walk_trusted_start_temp_directory.path())
                .unwrap()
                .uid(),
        );
    }

    #[test]
    fn host_directory_walk_rejects_mount_path_with_parent_directory_component() {
        use std::os::unix::fs::MetadataExt;
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        let mount_path_ancestor_directory = walk_trusted_start_temp_directory.path().join("srv");
        create_directory_with_mode(&mount_path_ancestor_directory, 0o755);
        create_directory_with_mode(&mount_path_ancestor_directory.join("scratch"), 0o770);

        assert_walk_fails_permanently_with_temp_trusted_start_dir_and_temp_dir_owner(
            walk_trusted_start_temp_directory.path(),
            &mount_path_ancestor_directory
                .join("..")
                .join("srv")
                .join("scratch"),
            std::fs::metadata(walk_trusted_start_temp_directory.path())
                .unwrap()
                .uid(),
        );
    }

    #[test]
    fn walk_refuses_mount_path_when_ancestor_has_different_owner() {
        use std::os::unix::fs::MetadataExt;
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        let mount_path_ancestor_directory = walk_trusted_start_temp_directory.path().join("srv");
        create_directory_with_mode(&mount_path_ancestor_directory, 0o755);
        create_directory_with_mode(&mount_path_ancestor_directory.join("scratch"), 0o770);

        assert_walk_fails_permanently_with_temp_trusted_start_dir_and_temp_dir_owner(
            walk_trusted_start_temp_directory.path(),
            &mount_path_ancestor_directory.join("scratch"),
            std::fs::metadata(walk_trusted_start_temp_directory.path())
                .unwrap()
                .uid()
                + 1,
        );
    }

    #[test]
    fn host_directory_walk_rejects_mount_path_equal_to_walk_start_directory() {
        use std::os::unix::fs::MetadataExt;
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();

        assert_walk_fails_permanently_with_temp_trusted_start_dir_and_temp_dir_owner(
            walk_trusted_start_temp_directory.path(),
            walk_trusted_start_temp_directory.path(),
            std::fs::metadata(walk_trusted_start_temp_directory.path())
                .unwrap()
                .uid(),
        );
    }

    #[test]
    fn test_directory_check_refuses_root_owned_and_nonempty() {
        let example_job_directory_path = Path::new("/srv/scratch/job");

        assert!(
            can_hand_job_directory_to_worker_user(
                example_job_directory_path,
                false,
                0,
                1000,
                0,
                true
            )
            .is_err()
        );
        assert!(
            can_hand_job_directory_to_worker_user(
                example_job_directory_path,
                false,
                1001,
                1000,
                0,
                true
            )
            .is_err()
        );
        assert!(
            can_hand_job_directory_to_worker_user(
                example_job_directory_path,
                true,
                0,
                1000,
                0,
                false
            )
            .is_err()
        );
        assert!(
            can_hand_job_directory_to_worker_user(
                example_job_directory_path,
                true,
                1000,
                1000,
                0,
                true
            )
            .is_err()
        );
        assert!(
            can_hand_job_directory_to_worker_user(
                example_job_directory_path,
                false,
                1000,
                1000,
                0,
                false
            )
            .is_ok()
        );
        assert!(
            can_hand_job_directory_to_worker_user(
                example_job_directory_path,
                true,
                0,
                1000,
                0,
                true
            )
            .is_ok()
        );
    }

    #[test]
    fn existing_worker_owned_job_directory_is_accepted_and_keeps_files() {
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        let guest_id = GuestId::new();
        let mut config = worker_config();
        config.mounts[2].path = walk_trusted_start_temp_directory.path().join("scratch");
        create_directory_with_mode(&config.mounts[2].path, 0o770);
        create_directory_with_mode(&config.mounts[2].host_directory_for_guest(&guest_id), 0o750);
        std::fs::write(
            config.mounts[2]
                .host_directory_for_guest(&guest_id)
                .join("result"),
            b"",
        )
        .unwrap();

        open_test_scratch_mount_with_temp_trusted_start_and_test_user_owner(
            &config,
            &guest_id,
            walk_trusted_start_temp_directory.path(),
        )
        .unwrap();
        assert!(
            config.mounts[2]
                .host_directory_for_guest(&guest_id)
                .join("result")
                .exists()
        );
    }

    #[test]
    fn creates_missing_job_mount_directory_inside_mount_path() {
        let walk_trusted_start_temp_directory = create_trusted_path_start_temp_dir();
        let mount_path_ancestor_directory = walk_trusted_start_temp_directory.path().join("srv");
        create_directory_with_mode(&mount_path_ancestor_directory, 0o755);
        let guest_id = GuestId::new();
        let mut config = worker_config();
        config.mounts[2].path = mount_path_ancestor_directory.join("scratch");
        create_directory_with_mode(&config.mounts[2].path, 0o770);

        open_test_scratch_mount_with_temp_trusted_start_and_test_user_owner(
            &config,
            &guest_id,
            walk_trusted_start_temp_directory.path(),
        )
        .unwrap();
        assert!(
            std::fs::symlink_metadata(config.mounts[2].host_directory_for_guest(&guest_id))
                .unwrap()
                .is_dir()
        );
    }

    #[test]
    fn only_guest_taps_on_the_guest_bridge_are_made() {
        let tap = |name: &str, bridge: &str| {
            build_prepare_tap_request(name, bridge, Some(Ipv4Addr::new(10, 233, 0, 2)))
        };
        assert!(allowed(tap("gr-tap-7", "gradient-guests")));
        assert!(!allowed(tap("gr-tap-7", "br-lan")));
        assert!(!allowed(tap("eth0", "gradient-guests")));
        assert!(!allowed(tap("gr-tap-", "gradient-guests")));
        assert!(!allowed(tap("gr-tap-123456789", "gradient-guests")));
    }

    #[test]
    fn rejects_prepare_tap_with_guest_ipv4_outside_guest_subnet() {
        assert!(!allowed(build_prepare_tap_request(
            "gr-tap-7",
            "gradient-guests",
            Some(Ipv4Addr::new(10, 233, 1, 2))
        )));
        assert!(!allowed(build_prepare_tap_request(
            "gr-tap-7",
            "gradient-guests",
            Some(Ipv4Addr::new(192, 168, 1, 10))
        )));
    }

    #[test]
    fn supervisor_rejects_prepare_tap_for_bridge_network_or_broadcast_guest_ipv4_address() {
        let test_guest_subnet_gateway_address = Ipv4Addr::new(10, 233, 0, 1);
        for guest_ipv4_address_that_check_must_refuse in [
            test_guest_subnet_gateway_address,
            Ipv4Addr::new(10, 233, 0, 0),
            Ipv4Addr::new(10, 233, 0, 255),
        ] {
            assert!(!allowed(build_prepare_tap_request(
                "gr-tap-7",
                "gradient-guests",
                Some(guest_ipv4_address_that_check_must_refuse)
            )));
        }
    }

    #[test]
    fn test_prepare_tap_accepts_usable_guest_ipv4_in_guest_subnet_and_rejects_missing_address() {
        for usable_guest_ipv4_address in
            [Ipv4Addr::new(10, 233, 0, 2), Ipv4Addr::new(10, 233, 0, 254)]
        {
            assert!(allowed(build_prepare_tap_request(
                "gr-tap-7",
                "gradient-guests",
                Some(usable_guest_ipv4_address)
            )));
        }
        assert!(!allowed(build_prepare_tap_request(
            "gr-tap-7",
            "gradient-guests",
            None
        )));
        let mut supervisor_config_without_guest_subnet = worker_config();
        supervisor_config_without_guest_subnet.guest_bridge_ipv4_subnet = None;
        assert!(
            check_request(
                &supervisor_config_without_guest_subnet,
                &[],
                &build_prepare_tap_request(
                    "gr-tap-7",
                    "gradient-guests",
                    Some(Ipv4Addr::new(10, 233, 0, 2))
                )
            )
            .is_err()
        );
    }

    #[test]
    fn test_bridge_isolation_and_nft_element_arguments() {
        let created_tap_guest_address =
            CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-70000",
                Ipv4Addr::new(10, 233, 0, 2),
                None,
            )
            .unwrap();
        assert_eq!(
            tap_isolation_link_set_arguments("gr-tap-70000"),
            [
                "link",
                "set",
                "dev",
                "gr-tap-70000",
                "type",
                "bridge_slave",
                "isolated",
                "on"
            ]
        );
        let nft_add_element_call_arguments =
            guest_tap_nft_add_element_arguments(&created_tap_guest_address);
        assert_eq!(
            nft_add_element_call_arguments,
            [
                "add",
                "element",
                "bridge",
                "gradient-guests",
                "guest-tap-addresses",
                "{ gr-tap-70000 . 02:47:52:01:11:70 . 10.233.0.2 }"
            ]
        );
        assert!(nft_add_element_call_arguments[5].contains(&guest_mac(70000)));
        let nft_delete_element_call_arguments = build_nft_call_arguments_for_tap_set_elements(
            "delete",
            &created_tap_guest_address.nftables_set_name_and_element_text_pairs(),
        );
        assert_eq!(
            nft_delete_element_call_arguments[..2],
            ["delete", "element"]
        );
        assert_eq!(
            nft_delete_element_call_arguments[2..],
            nft_add_element_call_arguments[2..]
        );
        let mut supervisor_config = worker_config();
        supervisor_config.guest_bridge_ipv6_subnet = Some("fd00:ec2::/64".parse().unwrap());
        let created_tap_guest_address_for_ipv6_guest =
            CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-70000",
                Ipv4Addr::new(10, 233, 0, 2),
                derive_guest_ipv6_address_from_supervisor_config_and_ipv4_address(
                    &supervisor_config,
                    Ipv4Addr::new(10, 233, 0, 2),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            guest_tap_nft_add_element_arguments(&created_tap_guest_address_for_ipv6_guest),
            [
                "add",
                "element",
                "bridge",
                "gradient-guests",
                "guest-tap-addresses",
                "{ gr-tap-70000 . 02:47:52:01:11:70 . 10.233.0.2 }",
                ";",
                "add",
                "element",
                "bridge",
                "gradient-guests",
                "guest-tap-ipv6-addresses",
                "{ gr-tap-70000 . 02:47:52:01:11:70 . fd00:ec2::2 }"
            ]
        );
        assert_eq!(
            build_nft_call_arguments_for_tap_set_elements(
                "delete",
                &created_tap_guest_address_for_ipv6_guest
                    .nftables_set_name_and_element_text_pairs()
            ),
            [
                "delete",
                "element",
                "bridge",
                "gradient-guests",
                "guest-tap-addresses",
                "{ gr-tap-70000 . 02:47:52:01:11:70 . 10.233.0.2 }",
                ";",
                "delete",
                "element",
                "bridge",
                "gradient-guests",
                "guest-tap-ipv6-addresses",
                "{ gr-tap-70000 . 02:47:52:01:11:70 . fd00:ec2::2 }"
            ]
        );
    }

    #[test]
    fn test_permanent_ipv6_neighbour_entry_arguments_name_guest_ipv6_address_mac_and_bridge() {
        let created_tap_guest_address_for_ipv6_guest =
            CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-7",
                Ipv4Addr::new(10, 233, 0, 5),
                Some("fd00:ec2::5".parse().unwrap()),
            )
            .unwrap();
        assert_eq!(
            permanent_ipv6_neighbor_entry_arguments_for_tap(
                &created_tap_guest_address_for_ipv6_guest,
                "gradient-guests"
            )
            .unwrap(),
            [
                "-6",
                "neigh",
                "replace",
                "fd00:ec2::5",
                "lladdr",
                "02:47:52:00:00:07",
                "dev",
                "gradient-guests",
                "nud",
                "permanent"
            ]
        );
    }

    #[test]
    fn ipv6_neighbour_delete_and_list_argument_builders_name_address_bridge_and_nud_permanent() {
        assert_eq!(
            build_ip_command_arguments_to_delete_ipv6_neighbor(
                "fd00:ec2::5".parse().unwrap(),
                "gradient-guests"
            ),
            [
                "-6",
                "neigh",
                "del",
                "fd00:ec2::5",
                "dev",
                "gradient-guests"
            ]
        );
        assert_eq!(
            build_ipv6_permanent_neighbor_show_arguments_for_bridge("gradient-guests"),
            [
                "-6",
                "neigh",
                "show",
                "dev",
                "gradient-guests",
                "nud",
                "permanent"
            ]
        );
    }

    #[test]
    fn test_plan_permanent_ipv6_neighbour_entries_on_guest_bridge_removes_stale_keeps_running_adds_missing_and_uses_newest_record()
     {
        let record = |seconds: u64, tap_interface_name: &str, ipv4: [u8; 4], ipv6: &str| {
            (
                std::time::UNIX_EPOCH + Duration::from_secs(seconds),
                CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                    tap_interface_name,
                    Ipv4Addr::from(ipv4),
                    Some(ipv6.parse().unwrap()),
                )
                .unwrap(),
            )
        };
        let ipv6_neighbor_show_stdout = [
            "fd00:ec2::2 lladdr 02:47:52:00:00:02 PERMANENT",
            "fd00:ec2::9 lladdr 02:47:52:00:00:09 PERMANENT",
            "fd00:ec2::4 lladdr 02:47:52:00:00:05 PERMANENT",
            "fd00:ec2::6 lladdr 02:47:52:00:00:07 PERMANENT",
            "fd00:ec2::8 lladdr 02:47:52:00:00:0a PERMANENT",
            "",
        ]
        .join("\n");
        let existing_tap_records_with_modification_time = vec![
            record(1, "gr-tap-2", [10, 233, 0, 2], "fd00:ec2::2"),
            record(2, "gr-tap-3", [10, 233, 0, 3], "fd00:ec2::3"),
            record(3, "gr-tap-10", [10, 233, 0, 8], "fd00:ec2::8"),
            record(4, "gr-tap-7", [10, 233, 0, 4], "fd00:ec2::4"),
            record(0, "gr-tap-5", [10, 233, 0, 4], "fd00:ec2::4"),
        ];
        let delete = |address: &str| {
            build_ip_command_arguments_to_delete_ipv6_neighbor(
                address.parse().unwrap(),
                "gradient-guests",
            )
        };
        let add = |(_, tap): &(std::time::SystemTime, CreatedTapGuestAddress)| {
            permanent_ipv6_neighbor_entry_arguments_for_tap(tap, "gradient-guests").unwrap()
        };
        assert_eq!(
            ip_arguments_to_sync_bridge_neighbour_entries_with_tap_records(
                &ipv6_neighbor_show_stdout,
                existing_tap_records_with_modification_time.clone(),
                "gradient-guests"
            ),
            [
                delete("fd00:ec2::9"),
                delete("fd00:ec2::4"),
                delete("fd00:ec2::6"),
                add(&existing_tap_records_with_modification_time[1]),
                add(&existing_tap_records_with_modification_time[3]),
            ]
        );
    }

    #[test]
    fn supervisor_returns_error_when_deriving_guest_ipv6_address_for_guest_ipv4_address_outside_ipv4_subnet_with_both_guest_subnets_configured()
     {
        let mut supervisor_config = worker_config();
        supervisor_config.guest_bridge_ipv6_subnet = Some("fd00:ec2::/112".parse().unwrap());
        for outside_guest_ipv4_subnet_address_text in
            ["10.232.255.255", "10.234.0.0", "192.168.1.5"]
        {
            assert!(
                derive_guest_ipv6_address_from_supervisor_config_and_ipv4_address(
                    &supervisor_config,
                    outside_guest_ipv4_subnet_address_text.parse().unwrap(),
                )
                .is_err(),
                "{outside_guest_ipv4_subnet_address_text}"
            );
        }
    }

    #[test]
    fn guest_without_ipv6_address_has_no_ipv6_set_element_or_neighbour_command_and_only_ipv4_nft_add_call()
     {
        let created_tap_guest_address =
            CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-7",
                Ipv4Addr::new(10, 233, 0, 5),
                derive_guest_ipv6_address_from_supervisor_config_and_ipv4_address(
                    &worker_config(),
                    Ipv4Addr::new(10, 233, 0, 5),
                )
                .unwrap(),
            )
            .unwrap();
        assert_eq!(
            created_tap_guest_address.nftables_set_element_ipv6_text(),
            None
        );
        assert_eq!(
            permanent_ipv6_neighbor_entry_arguments_for_tap(
                &created_tap_guest_address,
                "gradient-guests"
            ),
            None
        );
        assert_eq!(
            guest_tap_nft_add_element_arguments(&created_tap_guest_address),
            [
                "add",
                "element",
                "bridge",
                "gradient-guests",
                "guest-tap-addresses",
                "{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }"
            ]
        );
    }

    #[test]
    fn record_file_without_guest_ipv6_parses_and_record_with_guest_ipv6_round_trips() {
        let guest_id = GuestId::new();
        let record = TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
            &format!("{{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }}\n\n{guest_id}"),
            "gr-tap-7",
        )
        .unwrap();
        assert_eq!(
            record.tap_guest_address.guest_ipv4_address,
            Ipv4Addr::new(10, 233, 0, 5)
        );
        assert_eq!(record.tap_guest_address.guest_ipv6_address, None);
        assert_eq!(record.guest_id, guest_id);
        let record_for_ipv6_guest = TapAddressRecordWithGuestId {
            tap_guest_address: CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-7",
                Ipv4Addr::new(10, 233, 0, 5),
                Some("fd00:ec2::5".parse().unwrap()),
            )
            .unwrap(),
            guest_id: guest_id.clone(),
        };
        let supervisor_record_file_text = record_for_ipv6_guest.to_record_file_text();
        assert_eq!(
            supervisor_record_file_text,
            format!(
                "{{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }}\n{{ gr-tap-7 . 02:47:52:00:00:07 . fd00:ec2::5 }}\n{guest_id}"
            )
        );
        assert_eq!(
            TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
                &supervisor_record_file_text,
                "gr-tap-7"
            ),
            Some(record_for_ipv6_guest)
        );
        for record_text_that_must_be_refused in [
            format!(
                "{{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }}\n{{ gr-tap-7 . 02:47:52:00:00:08 . fd00:ec2::5 }}\n{guest_id}"
            ),
            format!(
                "{{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }}\n{{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.6 }}\n{guest_id}"
            ),
            format!(
                "{{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }}\n{{ gr-tap-7 . 02:47:52:00:00:07 . fd00:ec2::5 }}\n{guest_id}\n"
            ),
            "{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }\n\nnot-a-guest-id".to_owned(),
        ] {
            assert_eq!(
                TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
                    &record_text_that_must_be_refused,
                    "gr-tap-7"
                ),
                None,
                "{record_text_that_must_be_refused:?}"
            );
        }
    }

    #[test]
    fn guest_tap_address_record_file_requires_three_lines_and_reader_refuses_one_or_two_lines() {
        let record = TapAddressRecordWithGuestId {
            tap_guest_address: CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-7",
                Ipv4Addr::new(10, 233, 0, 5),
                None,
            )
            .unwrap(),
            guest_id: GuestId::new(),
        };
        let supervisor_record_file_text = record.to_record_file_text();
        assert_eq!(supervisor_record_file_text.split('\n').count(), 3);
        assert_eq!(
            TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
                &supervisor_record_file_text,
                "gr-tap-7"
            ),
            Some(record)
        );
        for record_text_that_must_be_refused in [
            "{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }",
            "{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }\n{ gr-tap-7 . 02:47:52:00:00:07 . fd00:ec2::5 }",
        ] {
            assert_eq!(
                TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
                    record_text_that_must_be_refused,
                    "gr-tap-7"
                ),
                None,
                "{record_text_that_must_be_refused:?}"
            );
        }
    }

    #[test]
    fn record_path_is_built_only_for_guest_tap_names() {
        assert_eq!(
            tap_record_file_path(
                Path::new(GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH),
                "gr-tap-7"
            )
            .unwrap(),
            Path::new("/run/gradient-guest-tap-addresses/gr-tap-7")
        );
        for name_that_is_not_a_guest_tap_name in [
            "",
            "gr-tap-",
            "gr-tap-7/../x",
            "../gr-tap-7",
            "gr-tap-7.tmp",
            "gr-tap-123456789",
            "eth0",
        ] {
            assert!(
                tap_record_file_path(
                    Path::new(GUEST_TAP_MAC_IPV4_NFTABLES_SET_ELEMENT_DIRECTORY_PATH),
                    name_that_is_not_a_guest_tap_name
                )
                .is_err(),
                "{name_that_is_not_a_guest_tap_name:?}"
            );
        }
    }

    #[test]
    fn test_record_text_parses_back_only_for_same_tap_and_delete_arguments_name_old_element() {
        let existing_tap_record = TapAddressRecordWithGuestId {
            tap_guest_address: CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-7",
                Ipv4Addr::new(10, 233, 0, 5),
                None,
            )
            .unwrap(),
            guest_id: GuestId::new(),
        };
        let guest_id = &existing_tap_record.guest_id;
        let supervisor_record_file_text = existing_tap_record.to_record_file_text();
        assert_eq!(
            supervisor_record_file_text,
            format!("{{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }}\n\n{guest_id}")
        );
        let rebuilt_tap_record = TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
            &supervisor_record_file_text,
            "gr-tap-7",
        )
        .unwrap();
        assert_eq!(rebuilt_tap_record, existing_tap_record);
        assert_eq!(
            TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
                &supervisor_record_file_text,
                "gr-tap-8"
            ),
            None
        );
        for record_text_that_must_be_refused in [
            String::new(),
            format!("{{ gr-tap-7 . 02:47:52:00:00:08 . 10.233.0.5 }}\n\n{guest_id}"),
            format!(
                "{{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5, gr-tap-9 . 02:47:52:00:00:09 . 10.233.0.9 }}\n\n{guest_id}"
            ),
        ] {
            assert_eq!(
                TapAddressRecordWithGuestId::from_record_file_text_and_tap_name(
                    &record_text_that_must_be_refused,
                    "gr-tap-7"
                ),
                None,
                "{record_text_that_must_be_refused:?}"
            );
        }
        assert_eq!(
            build_nft_call_arguments_for_tap_set_elements(
                "delete",
                &rebuilt_tap_record
                    .tap_guest_address
                    .nftables_set_name_and_element_text_pairs()
            ),
            [
                "delete",
                "element",
                "bridge",
                "gradient-guests",
                "guest-tap-addresses",
                "{ gr-tap-7 . 02:47:52:00:00:07 . 10.233.0.5 }"
            ]
        );
        assert_eq!(
            build_nft_set_element_command_arguments(
                "get",
                GUEST_TAP_MAC_IPV4_NFTABLES_SET_NAME,
                &rebuilt_tap_record
                    .tap_guest_address
                    .nftables_set_element_text()
            )[..2],
            ["get", "element"]
        );
    }

    #[derive(Default)]
    struct FakeHostCommand {
        tap_device_names: Mutex<Vec<String>>,
        existing_nftables_set_elements: Mutex<Vec<(String, String)>>,
        program_calls: Mutex<Vec<Vec<String>>>,
        tap_device_names_for_ip_link_del_failure: Mutex<Vec<String>>,
        tap_device_names_where_bridge_attach_never_returns: Mutex<Vec<String>>,
        nft_add_element_fails: bool,
    }

    impl TapReleaseHostCommands for FakeHostCommand {
        async fn run_program_and_fail_on_non_zero_exit(
            &self,
            program: &str,
            arguments: &[String],
        ) -> Result<()> {
            lock(&self.program_calls).push(
                std::iter::once(program.to_owned())
                    .chain(arguments.iter().cloned())
                    .collect(),
            );
            match (program, arguments) {
                ("ip", [link, del, dev, name])
                    if link == "link" && del == "del" && dev == "dev" =>
                {
                    ensure!(
                        !lock(&self.tap_device_names_for_ip_link_del_failure).contains(name),
                        "cannot delete device {name}"
                    );
                    let mut tap_device_names = lock(&self.tap_device_names);
                    ensure!(tap_device_names.contains(name), "no device {name}");
                    tap_device_names.retain(|tap_device_name| tap_device_name != name);
                    Ok(())
                }
                ("ip", [tuntap, add, dev, name, ..])
                    if tuntap == "tuntap" && add == "add" && dev == "dev" =>
                {
                    lock(&self.tap_device_names).push(name.clone());
                    Ok(())
                }
                ("ip", [link, set, name, master, ..])
                    if link == "link" && set == "set" && master == "master" =>
                {
                    if lock(&self.tap_device_names_where_bridge_attach_never_returns).contains(name)
                    {
                        std::future::pending::<()>().await;
                    }
                    Ok(())
                }
                ("ip", [link, set, dev, ..]) if link == "link" && set == "set" && dev == "dev" => {
                    Ok(())
                }
                ("nft", [add, ..]) if add == "add" => {
                    ensure!(!self.nft_add_element_fails, "can't add {arguments:?}");
                    let mut existing_nftables_set_elements =
                        lock(&self.existing_nftables_set_elements);
                    for nft_command in arguments.split(|argument| argument == ";") {
                        let [_, _, _, _, set_name, element_text] = nft_command else {
                            bail!("unexpected nft call {nft_command:?}");
                        };
                        existing_nftables_set_elements
                            .push((set_name.clone(), element_text.clone()));
                    }
                    Ok(())
                }
                ("nft", _) => {
                    let mut existing_nftables_set_elements =
                        lock(&self.existing_nftables_set_elements);
                    let mut deleted_set_elements = Vec::new();
                    for nft_command in arguments.split(|argument| argument == ";") {
                        let [command, _, _, _, set_name, element_text] = nft_command else {
                            bail!("unexpected nft call {nft_command:?}");
                        };
                        let set_element = (set_name.clone(), element_text.clone());
                        ensure!(
                            existing_nftables_set_elements.contains(&set_element),
                            "no element {set_element:?}"
                        );
                        if command == "delete" {
                            deleted_set_elements.push(set_element);
                        }
                    }
                    existing_nftables_set_elements
                        .retain(|set_element| !deleted_set_elements.contains(set_element));
                    Ok(())
                }
                _ => bail!("unexpected call {program} {arguments:?}"),
            }
        }

        fn host_network_device_exists(&self, name: &str) -> bool {
            lock(&self.tap_device_names)
                .iter()
                .any(|tap_device_name| tap_device_name == name)
        }
    }

    fn write_tap_record_file_to_temp_record_dir_and_return_record(
        record_directory: &Path,
        guest_vsock_context_id: u32,
        guest_id: &GuestId,
    ) -> TapAddressRecordWithGuestId {
        let record = TapAddressRecordWithGuestId {
            tap_guest_address: CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                &format!("{TAP_PREFIX}{guest_vsock_context_id}"),
                Ipv4Addr::new(10, 233, 0, 5),
                Some("fd00:ec2::5".parse().unwrap()),
            )
            .unwrap(),
            guest_id: guest_id.clone(),
        };
        std::fs::write(
            record_directory.join(&record.tap_guest_address.tap_interface_name),
            record.to_record_file_text(),
        )
        .unwrap();
        record
    }

    fn build_fake_host_command_with_existing_tap_device_and_record_set_elements(
        records: &[&TapAddressRecordWithGuestId],
    ) -> FakeHostCommand {
        FakeHostCommand {
            tap_device_names: Mutex::new(
                records
                    .iter()
                    .map(|record| record.tap_guest_address.tap_interface_name.clone())
                    .collect(),
            ),
            existing_nftables_set_elements: Mutex::new(
                records
                    .iter()
                    .flat_map(|record| {
                        record
                            .tap_guest_address
                            .nftables_set_name_and_element_text_pairs()
                    })
                    .map(|(set_name, element_text)| (set_name.to_owned(), element_text))
                    .collect(),
            ),
            ..FakeHostCommand::default()
        }
    }

    fn create_guest_runtime_directory_under_temp_base(
        guest_runtime_base_directory_path: &Path,
        guest_id: &GuestId,
    ) {
        std::fs::create_dir(guest_runtime_base_directory_path.join(guest_id.to_string())).unwrap();
    }

    #[tokio::test]
    async fn releasing_taps_for_one_guest_twice_succeeds_and_second_run_runs_no_command() {
        let record_directory = tempfile::tempdir().unwrap();
        let guest_id = GuestId::new();
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            7,
            &guest_id,
        );
        let host =
            build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[&record]);
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            record_directory.path(),
            Some(&guest_id),
        )
        .await
        .unwrap();
        assert!(!record_directory.path().join("gr-tap-7").exists());
        assert!(lock(&host.tap_device_names).is_empty());
        assert!(lock(&host.existing_nftables_set_elements).is_empty());
        lock(&host.program_calls).clear();
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            record_directory.path(),
            Some(&guest_id),
        )
        .await
        .unwrap();
        assert!(lock(&host.program_calls).is_empty());
    }

    #[tokio::test]
    async fn release_taps_succeeds_and_removes_record_file_and_nftables_set_elements_when_tap_device_is_already_gone()
     {
        let record_directory = tempfile::tempdir().unwrap();
        let guest_id = GuestId::new();
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            7,
            &guest_id,
        );
        let host =
            build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[&record]);
        lock(&host.tap_device_names).clear();
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            record_directory.path(),
            Some(&guest_id),
        )
        .await
        .unwrap();
        assert!(!record_directory.path().join("gr-tap-7").exists());
        assert!(lock(&host.existing_nftables_set_elements).is_empty());
        assert!(
            lock(&host.program_calls)
                .iter()
                .all(|program_call| program_call[0] == "nft")
        );
    }

    #[tokio::test]
    async fn release_taps_succeeds_without_running_command_when_record_directory_missing() {
        let record_directory = tempfile::tempdir().unwrap();
        let host = FakeHostCommand::default();
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            &record_directory.path().join("missing"),
            record_directory.path(),
            Some(&GuestId::new()),
        )
        .await
        .unwrap();
        assert!(lock(&host.program_calls).is_empty());
    }

    #[tokio::test]
    async fn release_taps_of_one_guest_does_not_change_tap_set_elements_or_record_file_for_another_guest_id()
     {
        let record_directory = tempfile::tempdir().unwrap();
        let guest_id = GuestId::new();
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            7,
            &guest_id,
        );
        let record_of_another_guest = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            8,
            &GuestId::new(),
        );
        create_guest_runtime_directory_under_temp_base(
            record_directory.path(),
            &record_of_another_guest.guest_id,
        );
        let host = build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[
            &record,
            &record_of_another_guest,
        ]);
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            record_directory.path(),
            Some(&guest_id),
        )
        .await
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(record_directory.path().join("gr-tap-8")).unwrap(),
            record_of_another_guest.to_record_file_text()
        );
        assert_eq!(*lock(&host.tap_device_names), ["gr-tap-8"]);
        assert_eq!(lock(&host.existing_nftables_set_elements).len(), 2);
        assert!(
            lock(&host.program_calls)
                .iter()
                .flatten()
                .all(|argument| !argument.contains("gr-tap-8"))
        );
    }

    #[tokio::test]
    async fn cleanup_releases_tap_device_set_elements_and_record_file_when_record_file_does_not_parse_for_any_guest_id()
     {
        let record_directory = tempfile::tempdir().unwrap();
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            9,
            &GuestId::new(),
        );
        let record_file_path = record_directory.path().join("gr-tap-9");
        let text_without_guest_id_line = std::fs::read_to_string(&record_file_path)
            .unwrap()
            .rsplit_once('\n')
            .unwrap()
            .0
            .to_owned();
        std::fs::write(&record_file_path, text_without_guest_id_line).unwrap();
        let host =
            build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[&record]);
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            record_directory.path(),
            Some(&GuestId::new()),
        )
        .await
        .unwrap();
        assert!(!record_file_path.exists());
        assert!(lock(&host.tap_device_names).is_empty());
        assert!(lock(&host.existing_nftables_set_elements).is_empty());
    }

    #[tokio::test]
    async fn releasing_tap_record_of_earlier_guest_before_preparing_new_guest_tap_removes_device_both_set_elements_and_record_file()
     {
        let record_directory = tempfile::tempdir().unwrap();
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            7,
            &GuestId::new(),
        );
        let host =
            build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[&record]);
        delete_leftover_tap_record_nft_elements_file_and_device_for_tap_name(
            &host,
            record_directory.path(),
            "gr-tap-7",
        )
        .await
        .unwrap();
        assert!(!record_directory.path().join("gr-tap-7").exists());
        assert!(lock(&host.tap_device_names).is_empty());
        assert!(lock(&host.existing_nftables_set_elements).is_empty());
    }

    #[tokio::test]
    async fn tap_record_stays_when_guest_runtime_directory_exists_during_other_guest_tap_prepare_and_third_guest_cleanup()
     {
        let record_directory = tempfile::tempdir().unwrap();
        let temporary_guest_runtime_base_directory = tempfile::tempdir().unwrap();
        let (starting_guest_id, tap_preparing_guest_id, cleanup_running_guest_id) =
            (GuestId::new(), GuestId::new(), GuestId::new());
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            7,
            &starting_guest_id,
        );
        for guest_id in [
            &starting_guest_id,
            &tap_preparing_guest_id,
            &cleanup_running_guest_id,
        ] {
            create_guest_runtime_directory_under_temp_base(
                temporary_guest_runtime_base_directory.path(),
                guest_id,
            );
        }
        let host =
            build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[&record]);
        for stopped_guest_id in [None, Some(&cleanup_running_guest_id)] {
            release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
                &host,
                record_directory.path(),
                temporary_guest_runtime_base_directory.path(),
                stopped_guest_id,
            )
            .await
            .unwrap();
        }
        assert_eq!(
            std::fs::read_to_string(record_directory.path().join("gr-tap-7")).unwrap(),
            record.to_record_file_text()
        );
        assert_eq!(*lock(&host.tap_device_names), ["gr-tap-7"]);
        assert_eq!(lock(&host.existing_nftables_set_elements).len(), 2);
        assert!(lock(&host.program_calls).is_empty());
    }

    #[tokio::test]
    async fn failed_cleanup_retried_by_next_lock_holder_removes_tap_record_and_neighbour_entry() {
        let record_directory = tempfile::tempdir().unwrap();
        let temporary_guest_runtime_base_directory = tempfile::tempdir().unwrap();
        let gone_guest_id = GuestId::new();
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            7,
            &gone_guest_id,
        );
        let host =
            build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[&record]);
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            temporary_guest_runtime_base_directory.path(),
            None,
        )
        .await
        .unwrap();
        assert!(!record_directory.path().join("gr-tap-7").exists());
        assert!(lock(&host.tap_device_names).is_empty());
        assert!(lock(&host.existing_nftables_set_elements).is_empty());
        let guest_ipv6_address_from_tap_record =
            record.tap_guest_address.guest_ipv6_address.unwrap();
        let fake_ip6_neigh_show_output_for_gone_guest = format!(
            "{guest_ipv6_address_from_tap_record} lladdr {} PERMANENT\n",
            guest_mac(record.tap_guest_address.guest_vsock_context_id)
        );
        let bridge_neighbour_sync_ip_commands =
            ip_arguments_to_sync_bridge_neighbour_entries_with_tap_records(
                &fake_ip6_neigh_show_output_for_gone_guest,
                read_all_tap_record_files_with_modification_times_for_existing_tap_devices(
                    &host,
                    record_directory.path(),
                )
                .unwrap(),
                "gradient-guests",
            );
        assert_eq!(
            bridge_neighbour_sync_ip_commands,
            [build_ip_command_arguments_to_delete_ipv6_neighbor(
                guest_ipv6_address_from_tap_record,
                "gradient-guests"
            )]
        );
    }

    #[tokio::test]
    async fn cleanup_releases_next_tap_and_returns_first_error_when_first_tap_delete_fails() {
        let record_directory = tempfile::tempdir().unwrap();
        let guest_id = GuestId::new();
        let records = [7, 8].map(|guest_vsock_context_id| {
            write_tap_record_file_to_temp_record_dir_and_return_record(
                record_directory.path(),
                guest_vsock_context_id,
                &guest_id,
            )
        });
        let host = build_fake_host_command_with_existing_tap_device_and_record_set_elements(
            &records.iter().collect::<Vec<_>>(),
        );
        lock(&host.tap_device_names_for_ip_link_del_failure).push("gr-tap-7".to_owned());
        let error =
            release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
                &host,
                record_directory.path(),
                record_directory.path(),
                Some(&guest_id),
            )
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("gr-tap-7"));
        assert_eq!(*lock(&host.tap_device_names), ["gr-tap-7"]);
        assert!(record_directory.path().join("gr-tap-7").exists());
        assert!(!record_directory.path().join("gr-tap-8").exists());
        assert_eq!(lock(&host.existing_nftables_set_elements).len(), 0);
    }

    #[tokio::test]
    async fn test_release_keeps_tap_record_when_device_delete_fails_after_set_elements_removed() {
        let record_directory = tempfile::tempdir().unwrap();
        let guest_id = GuestId::new();
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            7,
            &guest_id,
        );
        let host =
            build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[&record]);
        lock(&host.tap_device_names_for_ip_link_del_failure).push("gr-tap-7".to_owned());
        delete_tap_record_nft_elements_file_and_device(
            &host,
            record_directory.path(),
            "gr-tap-7",
            &record
                .tap_guest_address
                .nftables_set_name_and_element_text_pairs(),
        )
        .await
        .unwrap_err();
        assert_eq!(
            std::fs::read_to_string(record_directory.path().join("gr-tap-7")).unwrap(),
            record.to_record_file_text()
        );
        assert_eq!(*lock(&host.tap_device_names), ["gr-tap-7"]);
        assert!(lock(&host.existing_nftables_set_elements).is_empty());
        lock(&host.tap_device_names_for_ip_link_del_failure).clear();
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            record_directory.path(),
            Some(&guest_id),
        )
        .await
        .unwrap();
        assert!(!record_directory.path().join("gr-tap-7").exists());
        assert!(lock(&host.tap_device_names).is_empty());
    }

    #[tokio::test]
    async fn stopped_tap_preparation_keeps_record_device_and_elements_until_next_lock_holder_sees_guest_gone()
     {
        let record_directory = tempfile::tempdir().unwrap();
        let temporary_guest_runtime_base_directory = tempfile::tempdir().unwrap();
        let gone_guest_id = GuestId::new();
        create_guest_runtime_directory_under_temp_base(
            temporary_guest_runtime_base_directory.path(),
            &gone_guest_id,
        );
        let record = TapAddressRecordWithGuestId {
            tap_guest_address: CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-7",
                Ipv4Addr::new(10, 233, 0, 5),
                Some("fd00:ec2::5".parse().unwrap()),
            )
            .unwrap(),
            guest_id: gone_guest_id.clone(),
        };
        let host = FakeHostCommand::default();
        lock(&host.tap_device_names_where_bridge_attach_never_returns).push("gr-tap-7".to_owned());
        assert!(
            tokio::time::timeout(
                Duration::from_millis(100),
                write_tap_record_then_create_tap_device(
                    &host,
                    record_directory.path(),
                    &record,
                    "gradient-guests",
                    ("1000", "1000"),
                    false,
                ),
            )
            .await
            .is_err()
        );
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            temporary_guest_runtime_base_directory.path(),
            None,
        )
        .await
        .unwrap();
        assert!(record_directory.path().join("gr-tap-7").exists());
        assert_eq!(*lock(&host.tap_device_names), ["gr-tap-7"]);
        std::fs::remove_dir(
            temporary_guest_runtime_base_directory
                .path()
                .join(gone_guest_id.to_string()),
        )
        .unwrap();
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            temporary_guest_runtime_base_directory.path(),
            None,
        )
        .await
        .unwrap();
        assert!(!record_directory.path().join("gr-tap-7").exists());
        assert!(lock(&host.tap_device_names).is_empty());
        assert!(lock(&host.existing_nftables_set_elements).is_empty());
    }

    #[tokio::test]
    async fn tap_preparation_nftables_set_element_add_failure_removes_tap_record_set_element_and_tap_device()
     {
        let record_directory = tempfile::tempdir().unwrap();
        let record = TapAddressRecordWithGuestId {
            tap_guest_address: CreatedTapGuestAddress::from_checked_tap_name_and_guest_ipv4(
                "gr-tap-7",
                Ipv4Addr::new(10, 233, 0, 5),
                Some("fd00:ec2::5".parse().unwrap()),
            )
            .unwrap(),
            guest_id: GuestId::new(),
        };
        let host = FakeHostCommand {
            nft_add_element_fails: true,
            ..FakeHostCommand::default()
        };
        write_tap_record_then_create_tap_device(
            &host,
            record_directory.path(),
            &record,
            "gradient-guests",
            ("1000", "1000"),
            false,
        )
        .await
        .unwrap_err();
        assert!(!record_directory.path().join("gr-tap-7").exists());
        assert!(lock(&host.tap_device_names).is_empty());
        assert!(lock(&host.existing_nftables_set_elements).is_empty());
    }

    #[tokio::test]
    async fn command_exceeding_time_limit_is_killed_and_reaped_and_error_names_command() {
        let process_id_file_path = tempfile::NamedTempFile::new().unwrap();
        let error = run_program_and_get_stdout_with_time_limit(
            "sh",
            &[
                "-c",
                &format!(
                    "echo $$ > {}; exec sleep 3",
                    process_id_file_path.path().display()
                ),
            ],
            Duration::from_millis(500),
        )
        .await
        .unwrap_err();
        assert!(error.to_string().starts_with("sh -c"));
        let process_id_text_from_temp_file =
            std::fs::read_to_string(process_id_file_path.path()).unwrap();
        assert!(
            !Path::new("/proc")
                .join(process_id_text_from_temp_file.trim())
                .exists()
        );
    }

    #[tokio::test]
    async fn unreadable_tap_record_directory_is_skipped_and_cleanup_and_ipv6_neighbour_sync_still_read_other_records()
     {
        let record_directory = tempfile::tempdir().unwrap();
        std::fs::create_dir(record_directory.path().join("gr-tap-5")).unwrap();
        let record = write_tap_record_file_to_temp_record_dir_and_return_record(
            record_directory.path(),
            6,
            &GuestId::new(),
        );
        let host =
            build_fake_host_command_with_existing_tap_device_and_record_set_elements(&[&record]);
        lock(&host.tap_device_names).push("gr-tap-5".to_owned());
        assert_eq!(
            read_all_tap_record_files_with_modification_times_for_existing_tap_devices(
                &host,
                record_directory.path(),
            )
            .unwrap()
            .len(),
            1
        );
        release_taps_for_stopping_guest_missing_runtime_directory_and_unparsable_records(
            &host,
            record_directory.path(),
            record_directory.path(),
            None,
        )
        .await
        .unwrap();
        assert!(!record_directory.path().join("gr-tap-6").exists());
        assert_eq!(*lock(&host.tap_device_names), ["gr-tap-5"]);
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
