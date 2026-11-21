/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::os::fd::OwnedFd;
use std::os::unix::fs::{PermissionsExt, chown};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use gradient_redact::SecretRedactor;
use gradient_util::shutdown::Shutdown;
use gradient_wire::guest::{GuestMount, GuestNetwork, GuestRelay, GuestRelayKind, RunJob};
use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{Group, User};
use tokio::fs::File;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener};

use crate::cli_config::write_cli_config;
use crate::context::{context_env, context_json};
use crate::redaction::job_redactor;
use crate::run::{WORK_DIR, program_command};

pub const PROGRAM_USER: &str = "gradient";
const CONTEXT_FILE: &str = "/run/gradient/context.json";
const SECRETS_DIR: &str = "/run/gradient/secrets";
const PHASE_PAYLOAD_FILE: &str = "/run/gradient/phase-payload";
const RESOLV_CONF: &str = "/etc/resolv.conf";
const DEVICE_LINK_DIR: &str = "/dev/gradient";
const DEVICE_GROUP: &str = "dialout";
const INTERFACE_WAIT: Duration = Duration::from_secs(60);
const INTERFACE_POLL: Duration = Duration::from_millis(100);
const NET_CLASS_DIR: &str = "/sys/class/net";
const WORK_DISK: &str = "/dev/disk/by-id/virtio-work";

pub struct PreparedJob {
    pub program: Command,
    pub env: BTreeMap<String, String>,
    pub terminal: bool,
    pub wait_for_terminal: bool,
    pub redactor: SecretRedactor,
    pub start_commit: String,
}

pub async fn prepare(job: &RunJob, tasks: &Shutdown) -> Result<PreparedJob, String> {
    prepare_steps(job, tasks)
        .await
        .map_err(|e| format!("{e:#}"))
}

async fn prepare_steps(job: &RunJob, tasks: &Shutdown) -> Result<PreparedJob> {
    let env = job_env(job);
    let program = program_command(job, &env)?;
    write_context(job).await?;
    write_cli_config(&job.context).await?;
    write_secrets(job).await?;
    write_phase_payload(job).await?;
    if job.work_disk {
        mount_work_disk(Path::new(WORK_DISK)).await?;
    }
    mount_shares(&job.mounts).await?;
    if let Some(network) = &job.network {
        configure_network(network).await?;
    }
    start_relays(&job.relays, tasks).await?;
    attach_usb(&job.usb_bus_ids).await?;
    Ok(PreparedJob {
        program,
        env,
        terminal: job.program_pty,
        wait_for_terminal: job.wait_for_terminal,
        redactor: job_redactor(job),
        start_commit: job.context.job.get("commit").cloned().unwrap_or_default(),
    })
}

fn job_env(job: &RunJob) -> BTreeMap<String, String> {
    let mut env = context_env(&job.context, &job.secrets);
    if !job.last_phase_payload.is_empty() {
        env.insert(
            "GRADIENT_PHASE_PAYLOAD_FILE".to_owned(),
            PHASE_PAYLOAD_FILE.to_owned(),
        );
    }
    env
}

async fn write_context(job: &RunJob) -> Result<()> {
    let json = serde_json::to_vec_pretty(&context_json(&job.context))?;
    write_file(Path::new(CONTEXT_FILE), &json, 0o400).await
}

async fn write_secrets(job: &RunJob) -> Result<()> {
    if job.secrets.is_empty() {
        return Ok(());
    }

    let dir = Path::new(SECRETS_DIR);
    tokio::fs::create_dir_all(dir).await?;
    give_to_program_user(dir, 0o500).await?;
    for secret in &job.secrets {
        write_file(&dir.join(&secret.name), secret.value.expose(), 0o400).await?;
    }
    Ok(())
}

async fn write_phase_payload(job: &RunJob) -> Result<()> {
    if job.last_phase_payload.is_empty() {
        return Ok(());
    }

    write_file(
        Path::new(PHASE_PAYLOAD_FILE),
        &job.last_phase_payload,
        0o400,
    )
    .await
}

pub async fn write_file(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(path, data)
        .await
        .with_context(|| format!("write {}", path.display()))?;
    give_to_program_user(path, mode).await
}

pub async fn give_to_program_user(path: &Path, mode: u32) -> Result<()> {
    let user = User::from_name(PROGRAM_USER)?
        .ok_or_else(|| anyhow!("user {PROGRAM_USER} does not exist"))?;
    chown(path, Some(user.uid.as_raw()), Some(user.gid.as_raw()))
        .with_context(|| format!("chown {}", path.display()))?;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).await?;
    Ok(())
}

async fn mount_shares(mounts: &[GuestMount]) -> Result<()> {
    for mount in mounts {
        tokio::fs::create_dir_all(&mount.path).await?;
        let mut args = vec!["-t", "virtiofs"];
        if mount.read_only {
            args.extend(["-o", "ro"]);
        }
        args.extend([mount.tag.as_str(), mount.path.as_str()]);
        run_command("mount", &args).await?;
    }
    Ok(())
}

async fn mount_work_disk(disk: &Path) -> Result<()> {
    let deadline = tokio::time::Instant::now() + INTERFACE_WAIT;
    while !tokio::fs::try_exists(disk).await.unwrap_or(false) {
        if tokio::time::Instant::now() >= deadline {
            bail!(
                "the work disk {} did not appear in the guest",
                disk.display()
            );
        }
        tokio::time::sleep(INTERFACE_POLL).await;
    }
    let disk = disk.to_string_lossy();
    tokio::fs::create_dir_all(WORK_DIR).await?;
    run_command("mount", &["-t", "ext4", &disk, WORK_DIR]).await?;
    let owner = format!("{PROGRAM_USER}:{PROGRAM_USER}");
    run_command("chown", &["-R", &owner, WORK_DIR]).await
}

#[derive(Debug, PartialEq)]
enum GuestNetworkSetupStep {
    WriteProcSysKernelSetting {
        ipv6_interface_proc_setting_file_path: PathBuf,
        kernel_setting_value: &'static str,
    },
    RunIpCommandWithArguments(Vec<String>),
}

fn build_guest_network_setup_steps_from_network_message(
    guest_network_interface_name: &str,
    network: &GuestNetwork,
) -> Vec<GuestNetworkSetupStep> {
    let make_ip_command_step = |arguments: &[&str]| {
        GuestNetworkSetupStep::RunIpCommandWithArguments(
            arguments
                .iter()
                .map(|&argument| argument.to_owned())
                .collect(),
        )
    };
    let mut network_setup_steps = Vec::new();
    if network.ipv6_configuration.is_some() {
        // Write these settings before link up. The kernel can create a link-local address, start duplicate address detection and accept router advertisements right after link up. The kernel does not create a link-local address with `addr_gen_mode` 1.
        for (ipv6_interface_setting_file_name, kernel_setting_value) in [
            ("accept_ra", "0"),
            ("accept_dad", "0"),
            ("addr_gen_mode", "1"),
        ] {
            network_setup_steps.push(GuestNetworkSetupStep::WriteProcSysKernelSetting {
                ipv6_interface_proc_setting_file_path: Path::new("/proc/sys/net/ipv6/conf")
                    .join(guest_network_interface_name)
                    .join(ipv6_interface_setting_file_name),
                kernel_setting_value,
            });
        }
    }
    let address = format!("{}/{}", network.address, network.prefix);
    network_setup_steps.extend([
        make_ip_command_step(&["link", "set", "dev", guest_network_interface_name, "up"]),
        make_ip_command_step(&["addr", "add", &address, "dev", guest_network_interface_name]),
        make_ip_command_step(&["route", "add", "default", "via", &network.gateway]),
    ]);
    if let Some(ipv6_configuration) = &network.ipv6_configuration {
        let address = format!(
            "{}/{}",
            ipv6_configuration.address, ipv6_configuration.prefix
        );
        network_setup_steps.extend([
            make_ip_command_step(&[
                "-6",
                "addr",
                "add",
                &address,
                "dev",
                guest_network_interface_name,
                "nodad",
            ]),
            make_ip_command_step(&[
                "-6",
                "route",
                "add",
                "default",
                "via",
                &ipv6_configuration.gateway,
            ]),
        ]);
    }
    network_setup_steps
}

async fn configure_network(network: &GuestNetwork) -> Result<()> {
    let interface = interface_with_mac(Path::new(NET_CLASS_DIR), &network.mac).await?;
    for network_setup_step in
        build_guest_network_setup_steps_from_network_message(&interface, network)
    {
        match network_setup_step {
            GuestNetworkSetupStep::WriteProcSysKernelSetting {
                ipv6_interface_proc_setting_file_path,
                kernel_setting_value,
            } => tokio::fs::write(&ipv6_interface_proc_setting_file_path, kernel_setting_value)
                .await
                .with_context(|| {
                    format!("write {}", ipv6_interface_proc_setting_file_path.display())
                })?,
            GuestNetworkSetupStep::RunIpCommandWithArguments(arguments) => {
                let arguments: Vec<&str> = arguments.iter().map(String::as_str).collect();
                run_command("ip", &arguments).await?;
            }
        }
    }
    write_resolv_conf(Path::new(RESOLV_CONF), &network.nameservers).await
}

async fn write_resolv_conf(path: &Path, nameservers: &[String]) -> Result<()> {
    if nameservers.is_empty() {
        return Ok(());
    }

    let lines: String = nameservers
        .iter()
        .map(|nameserver| format!("nameserver {nameserver}\n"))
        .collect();
    match tokio::fs::remove_file(path).await {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            return Err(e).with_context(|| format!("remove {}", path.display()));
        }
        _ => {}
    }
    tokio::fs::write(path, lines)
        .await
        .with_context(|| format!("write {}", path.display()))
}

async fn interface_with_mac(net_dir: &Path, mac: &str) -> Result<String> {
    let deadline = tokio::time::Instant::now() + INTERFACE_WAIT;
    let mut previous = None;
    loop {
        let found = interface_named_by_mac(net_dir, mac)?;
        if let Some(name) = found.as_ref().filter(|_| found == previous) {
            return Ok(name.clone());
        }

        if tokio::time::Instant::now() >= deadline {
            bail!("no network interface with MAC {mac} appeared in the guest");
        }
        previous = found;
        tokio::time::sleep(INTERFACE_POLL).await;
    }
}

fn interface_named_by_mac(net_dir: &Path, mac: &str) -> Result<Option<String>> {
    let mut names = std::fs::read_dir(net_dir)?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .filter(|name| name != "lo")
        .filter(|name| mac.is_empty() || has_mac(&net_dir.join(name), mac))
        .collect::<Vec<_>>();
    names.sort();
    Ok(names.into_iter().next())
}

fn has_mac(interface: &Path, mac: &str) -> bool {
    std::fs::read_to_string(interface.join("address"))
        .is_ok_and(|address| address.trim().eq_ignore_ascii_case(mac))
}

async fn start_relays(relays: &[GuestRelay], tasks: &Shutdown) -> Result<()> {
    for relay in relays {
        let vsock = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, relay.port))
            .with_context(|| format!("listen on vsock port {}", relay.port))?;
        match relay.kind {
            GuestRelayKind::Pty => {
                let pty = open_device_pty(&relay.name)?;
                tasks.spawn(relay_pty(vsock, pty));
            }
            GuestRelayKind::Tcp { listen } => {
                let tcp = TcpListener::bind(("127.0.0.1", listen))
                    .await
                    .with_context(|| format!("listen on 127.0.0.1:{listen}"))?;
                tasks.spawn(relay_tcp(vsock, tcp, tasks.clone()));
            }
        }
    }
    Ok(())
}

struct DevicePty {
    from_device: File,
    to_device: File,
    _slave: OwnedFd,
}

fn open_device_pty(name: &str) -> Result<DevicePty> {
    let pty = nix::pty::openpty(None, None)?;
    let mut termios = tcgetattr(&pty.slave)?;
    cfmakeraw(&mut termios);
    tcsetattr(&pty.slave, SetArg::TCSANOW, &termios)?;

    let slave_path = nix::unistd::ttyname(&pty.slave)?;
    let group = Group::from_name(DEVICE_GROUP)?
        .ok_or_else(|| anyhow!("group {DEVICE_GROUP} does not exist"))?;
    chown(&slave_path, None, Some(group.gid.as_raw()))?;
    std::fs::set_permissions(&slave_path, std::fs::Permissions::from_mode(0o660))?;

    let link = PathBuf::from(DEVICE_LINK_DIR).join(name);
    std::fs::create_dir_all(DEVICE_LINK_DIR)?;
    std::os::unix::fs::symlink(&slave_path, &link)
        .with_context(|| format!("link {}", link.display()))?;
    let to_device = File::from_std(pty.master.try_clone()?.into());
    Ok(DevicePty {
        from_device: File::from_std(pty.master.into()),
        to_device,
        _slave: pty.slave,
    })
}

async fn relay_pty(vsock: VsockListener, mut pty: DevicePty) {
    while let Ok((stream, _)) = vsock.accept().await {
        let (mut from_host, mut to_host) = tokio::io::split(stream);
        tokio::select! {
            _ = tokio::io::copy(&mut pty.from_device, &mut to_host) => {}
            _ = tokio::io::copy(&mut from_host, &mut pty.to_device) => {}
        }
    }
}

async fn relay_tcp(vsock: VsockListener, tcp: TcpListener, tasks: Shutdown) {
    while let Ok((mut local, _)) = tcp.accept().await {
        let Ok((mut host, _)) = vsock.accept().await else {
            return;
        };
        tasks.spawn(async move {
            let _ = tokio::io::copy_bidirectional(&mut local, &mut host).await;
        });
    }
}

async fn attach_usb(bus_ids: &[String]) -> Result<()> {
    for bus_id in bus_ids {
        run_command("usbip", &["attach", "-r", "127.0.0.1", "-b", bus_id]).await?;
    }
    Ok(())
}

async fn run_command(program: &str, args: &[&str]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .kill_on_drop(true)
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gradient_wire::guest::{GuestNetwork, GuestNetworkIpv6};

    use super::{
        GuestNetworkSetupStep, build_guest_network_setup_steps_from_network_message,
        interface_with_mac, write_resolv_conf,
    };

    fn build_example_guest_network_message_with_ipv4_and_optional_ipv6(
        ipv6_configuration: Option<GuestNetworkIpv6>,
    ) -> GuestNetwork {
        GuestNetwork {
            address: "10.233.0.2".to_owned(),
            prefix: 16,
            gateway: "10.233.0.1".to_owned(),
            nameservers: vec!["10.233.0.1".to_owned()],
            mac: "02:47:52:00:00:02".to_owned(),
            ipv6_configuration,
        }
    }

    fn expected_step_running_ip_with_arguments(arguments: &[&str]) -> GuestNetworkSetupStep {
        GuestNetworkSetupStep::RunIpCommandWithArguments(
            arguments
                .iter()
                .map(|&argument| argument.to_owned())
                .collect(),
        )
    }

    fn expected_ipv4_steps_for_enp0s5_and_10_233_0_2_16() -> Vec<GuestNetworkSetupStep> {
        vec![
            expected_step_running_ip_with_arguments(&["link", "set", "dev", "enp0s5", "up"]),
            expected_step_running_ip_with_arguments(&[
                "addr",
                "add",
                "10.233.0.2/16",
                "dev",
                "enp0s5",
            ]),
            expected_step_running_ip_with_arguments(&[
                "route",
                "add",
                "default",
                "via",
                "10.233.0.1",
            ]),
        ]
    }

    #[test]
    fn test_ipv6_configuration_disables_router_advertisements_duplicate_address_detection_and_link_local_addresses_before_bringing_link_up_and_sets_static_address_and_default_route()
     {
        let network = build_example_guest_network_message_with_ipv4_and_optional_ipv6(Some(
            GuestNetworkIpv6 {
                address: "fd00:ec2:6::2".to_owned(),
                prefix: 64,
                gateway: "fd00:ec2:6::1".to_owned(),
            },
        ));
        let expected_ipv6_kernel_setting_write_step_for_enp0s5 =
            |name: &str, kernel_setting_value| GuestNetworkSetupStep::WriteProcSysKernelSetting {
                ipv6_interface_proc_setting_file_path: PathBuf::from(format!(
                    "/proc/sys/net/ipv6/conf/enp0s5/{name}"
                )),
                kernel_setting_value,
            };
        let mut expected_network_setup_steps = vec![
            expected_ipv6_kernel_setting_write_step_for_enp0s5("accept_ra", "0"),
            expected_ipv6_kernel_setting_write_step_for_enp0s5("accept_dad", "0"),
            expected_ipv6_kernel_setting_write_step_for_enp0s5("addr_gen_mode", "1"),
        ];
        expected_network_setup_steps.extend(expected_ipv4_steps_for_enp0s5_and_10_233_0_2_16());
        expected_network_setup_steps.push(expected_step_running_ip_with_arguments(&[
            "-6",
            "addr",
            "add",
            "fd00:ec2:6::2/64",
            "dev",
            "enp0s5",
            "nodad",
        ]));
        expected_network_setup_steps.push(expected_step_running_ip_with_arguments(&[
            "-6",
            "route",
            "add",
            "default",
            "via",
            "fd00:ec2:6::1",
        ]));

        assert_eq!(
            build_guest_network_setup_steps_from_network_message("enp0s5", &network),
            expected_network_setup_steps
        );
    }

    #[test]
    fn test_no_ipv6_config_only_sets_ipv4_link_address_and_route() {
        let network = build_example_guest_network_message_with_ipv4_and_optional_ipv6(None);

        assert_eq!(
            build_guest_network_setup_steps_from_network_message("enp0s5", &network),
            expected_ipv4_steps_for_enp0s5_and_10_233_0_2_16()
        );
    }

    #[tokio::test]
    async fn the_interface_is_found_by_its_mac() {
        let net = tempfile::tempdir().expect("tempdir");
        for (name, address) in [
            ("lo", "00:00:00:00:00:00"),
            ("eth0", "52:54:00:12:34:56"),
            ("enp0s5", "02:47:52:00:00:0A"),
        ] {
            std::fs::create_dir(net.path().join(name)).expect("interface dir");
            std::fs::write(
                net.path().join(name).join("address"),
                format!("{address}\n"),
            )
            .expect("address");
        }

        assert_eq!(
            interface_with_mac(net.path(), "02:47:52:00:00:0a")
                .await
                .expect("interface"),
            "enp0s5"
        );
    }

    #[tokio::test]
    async fn the_nameservers_replace_a_linked_resolv_conf() {
        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("static-resolv.conf");
        std::fs::write(&target, "nameserver 127.0.0.53\n").expect("target");
        let path = dir.path().join("resolv.conf");
        std::os::unix::fs::symlink(&target, &path).expect("symlink");

        write_resolv_conf(&path, &["10.233.0.1".to_owned(), "9.9.9.9".to_owned()])
            .await
            .expect("write");

        assert_eq!(
            std::fs::read_to_string(&path).expect("read"),
            "nameserver 10.233.0.1\nnameserver 9.9.9.9\n"
        );
        assert_eq!(
            std::fs::read_to_string(&target).expect("read"),
            "nameserver 127.0.0.53\n"
        );
    }
}
