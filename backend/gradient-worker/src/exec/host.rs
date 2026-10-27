/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use gradient_vm::{DeviceInventory, GuestPool, GuestUnits, SysfsProbe, plan_reattach, read_states};
use gradient_wire::types::ExecInventory;
use tokio_util::sync::CancellationToken;

use super::inventory::advertised;
use super::reattach::{ResumedJob, resumable};
use crate::config::{ExecArgs, WorkerConfig};
use crate::executor::AbortSignal;

pub struct ExecHost {
    pub pool: Arc<GuestPool>,
    pub devices: DeviceInventory,
    pub config: ExecArgs,
    pub worker_id: String,
    pub zone: Option<String>,
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
        let worker_exe = std::env::current_exe().context("locate the worker binary")?;
        let vm = config.exec.vm_config(config.guests_dir(), &worker_exe);
        let units = GuestUnits::connect().await?;
        let devices = DeviceInventory::from_config(&vm.devices, &SysfsProbe);
        Ok(Arc::new(ExecHost {
            pool: GuestPool::new(vm, units),
            devices,
            config: config.exec.clone(),
            worker_id,
            zone: config.zone.clone().filter(|zone| !zone.is_empty()),
            worker_stop,
        }))
    }

    async fn reattach(&self) -> Result<Vec<ResumedJob>> {
        let units = self.pool.units().list().await?;
        let states = read_states(&self.pool.config().guests_dir);
        let plan = plan_reattach(&units, states);
        let busy = gradient_vm::reattach::apply(plan, &self.pool, &self.devices).await;
        Ok(resumable(busy, self.pool.units()).await)
    }

    pub fn inventory(&self) -> ExecInventory {
        advertised(
            &self.devices,
            self.config.labels.clone(),
            &self.worker_id,
            &self.config.mounts,
            self.config.allow(),
            self.config.defaults(),
            self.pool.warm_count(),
        )
    }

    pub fn start_session(&self) {
        self.pool.set_upcoming_exec_jobs(0);
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

    pub fn leaves_guest_running(&self, abort: &AbortSignal) -> bool {
        !abort.was_requested()
            && !self.config.stop_guests_with_worker
            && self.worker_stop.is_cancelled()
    }
}
