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
use gradient_util::shutdown::Shutdown;
use gradient_wire::guest::{GuestMount, GuestNetwork, GuestRelay, GuestRelayKind, RunJob};
use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
use nix::unistd::{Group, User};
use tokio::fs::File;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener};

use crate::context::{context_env, context_json};
use crate::run::program_command;

pub const PROGRAM_USER: &str = "gradient";
const CONTEXT_FILE: &str = "/run/gradient/context.json";
const SECRETS_DIR: &str = "/run/gradient/secrets";
const PHASE_PAYLOAD_FILE: &str = "/run/gradient/phase-payload";
const DEVICE_LINK_DIR: &str = "/dev/gradient";
const DEVICE_GROUP: &str = "dialout";
const INTERFACE_WAIT: Duration = Duration::from_secs(10);

pub struct PreparedJob {
    pub program: Command,
    pub env: BTreeMap<String, String>,
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
    write_secrets(job).await?;
    write_phase_payload(job).await?;
    mount_shares(&job.mounts).await?;
    if let Some(network) = &job.network {
        configure_network(network).await?;
    }
    start_relays(&job.relays, tasks).await?;
    attach_usb(&job.usb_bus_ids).await?;
    Ok(PreparedJob { program, env })
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
    write_file(Path::new(CONTEXT_FILE), &json, 0o644).await
}

async fn write_secrets(job: &RunJob) -> Result<()> {
    if job.secrets.is_empty() {
        return Ok(());
    }

    let dir = Path::new(SECRETS_DIR);
    tokio::fs::create_dir_all(dir).await?;
    give_to_program_user(dir, 0o500).await?;
    for secret in &job.secrets {
        write_file(&dir.join(&secret.name), &secret.value, 0o400).await?;
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

async fn write_file(path: &Path, data: &[u8], mode: u32) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }

    tokio::fs::write(path, data)
        .await
        .with_context(|| format!("write {}", path.display()))?;
    give_to_program_user(path, mode).await
}

async fn give_to_program_user(path: &Path, mode: u32) -> Result<()> {
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

async fn configure_network(network: &GuestNetwork) -> Result<()> {
    let interface = guest_interface().await?;
    let address = format!("{}/{}", network.address, network.prefix);
    run_command("ip", &["link", "set", "dev", &interface, "up"]).await?;
    run_command("ip", &["addr", "add", &address, "dev", &interface]).await?;
    run_command("ip", &["route", "add", "default", "via", &network.gateway]).await
}

async fn guest_interface() -> Result<String> {
    let deadline = tokio::time::Instant::now() + INTERFACE_WAIT;
    loop {
        let mut names = std::fs::read_dir("/sys/class/net")?
            .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
            .filter(|name| name != "lo")
            .collect::<Vec<_>>();
        names.sort();
        if let Some(name) = names.into_iter().next() {
            return Ok(name);
        }

        if tokio::time::Instant::now() >= deadline {
            bail!("no network interface appeared in the guest");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
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
