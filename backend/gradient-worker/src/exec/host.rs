/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use gradient_vm::{
    DeviceInventory, GuestAddresses, GuestBudget, GuestPool, GuestUnits, SysfsProbe, plan_reattach,
    read_states,
};
use gradient_wire::types::ExecInventory;
use tokio_util::sync::CancellationToken;
use tracing::info;

use super::inventory::advertised;
use super::reattach::{ResumedJob, resumable};
use super::sops::WorkerSecrets;
use crate::config::{ExecArgs, HostResources, WorkerConfig};

const BYTES_PER_MB: u64 = 1024 * 1024;

pub struct ExecHost {
    pub pool: Arc<GuestPool>,
    pub devices: DeviceInventory,
    pub addresses: GuestAddresses,
    pub config: ExecArgs,
    pub worker_id: String,
    pub zone: Option<String>,
    pub secrets: Arc<WorkerSecrets>,
    worker_stop: CancellationToken,
}

impl ExecHost {
    pub async fn start(
        config: &WorkerConfig,
        worker_id: String,
        worker_stop: CancellationToken,
    ) -> Result<(Arc<ExecHost>, Vec<ResumedJob>)> {
        let host = Self::new(config, worker_id, worker_stop).await?;
        let resumed = match host.config.stop_guests_with_worker {
            true => {
                host.pool.stop_every_unit().await;
                Vec::new()
            }
            false => host.reattach().await?,
        };
        host.pool.refill();
        Ok((host, resumed))
    }

    async fn new(
        config: &WorkerConfig,
        worker_id: String,
        worker_stop: CancellationToken,
    ) -> Result<Arc<ExecHost>> {
        if config.exec.image.is_none() {
            bail!("capabilities.exec needs exec.image, the guest image exec jobs boot");
        }
        let secrets = WorkerSecrets::open(
            &config.exec.sops_files,
            config.exec.sops_key_file.as_deref(),
        )?;
        let worker_exe = std::env::current_exe().context("locate the worker binary")?;
        let vm = config.exec.vm_config(config.guests_dir(), &worker_exe);
        let units = GuestUnits::connect().await?;
        let devices = DeviceInventory::from_config(&vm.devices, &SysfsProbe);
        let addresses = GuestAddresses::new(&config.exec.subnet, &config.exec.nameservers)
            .context("exec.subnet")?;
        let total = config
            .exec
            .guest_budget(&host_resources(&config.guests_dir())?);
        info!(
            cpus = total.cpus,
            memory_mb = total.memory_mb,
            disk_mb = total.disk_mb,
            "guest budget"
        );
        Ok(Arc::new(ExecHost {
            pool: GuestPool::new(vm, units, GuestBudget::new(total)),
            devices,
            addresses,
            config: config.exec.clone(),
            worker_id,
            zone: config.zone.clone().filter(|zone| !zone.is_empty()),
            secrets,
            worker_stop,
        }))
    }

    async fn reattach(&self) -> Result<Vec<ResumedJob>> {
        let units = self.pool.units().list().await?;
        let states = read_states(&self.pool.config().guests_dir);
        let plan = plan_reattach(&units, states);
        let busy =
            gradient_vm::reattach::apply(plan, &self.pool, &self.devices, &self.addresses).await;
        Ok(resumable(busy, self.pool.units()).await)
    }

    pub fn inventory(&self) -> ExecInventory {
        advertised(
            &self.devices,
            &self.config,
            &self.worker_id,
            self.pool.warm_guests(),
            self.secrets.names(),
            self.pool.capacity(),
        )
    }

    pub fn start_session(&self) {
        self.pool.want_warm_guests(Vec::new());
        self.pool.reopen();
    }

    pub async fn stop_resumed(&self, resumed: Vec<ResumedJob>) {
        for job in resumed {
            tracing::warn!(job_id = %job.stored.job_id, "no server URL to report a resumed exec job to, stopping its guest");
            job.guest.guest.teardown(self.pool.units()).await;
        }
    }

    pub async fn stop_with_worker(&self) {
        match self.config.stop_guests_with_worker {
            true => self.pool.stop_all_units().await,
            false => self.pool.stop_boots().await,
        }
    }

    pub fn worker_stop_keeping_guests(&self) -> CancellationToken {
        match self.config.stop_guests_with_worker {
            true => CancellationToken::new(),
            false => self.worker_stop.clone(),
        }
    }
}

fn host_resources(guests_dir: &Path) -> Result<HostResources> {
    std::fs::create_dir_all(guests_dir)
        .with_context(|| format!("create {}", guests_dir.display()))?;
    let disk = nix::sys::statvfs::statvfs(guests_dir)
        .with_context(|| format!("read the free space of {}", guests_dir.display()))?;
    let free_bytes = disk.blocks_available().saturating_mul(disk.fragment_size());
    let host = crate::metrics::host_static();
    Ok(HostResources {
        cpus: host.cpu_count,
        memory_mb: host.ram_total_mb,
        guests_dir_free_mb: free_bytes / BYTES_PER_MB,
    })
}
