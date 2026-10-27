/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use anyhow::{Context, Result, bail};
use gradient_vm::{DeviceInventory, GuestPool, GuestUnits, SysfsProbe};
use gradient_wire::types::ExecInventory;

use super::inventory::advertised;
use crate::config::{ExecArgs, WorkerConfig};
use crate::executor::AbortSignal;

pub struct ExecHost {
    pub pool: Arc<GuestPool>,
    pub devices: DeviceInventory,
    pub config: ExecArgs,
    pub worker_id: String,
    pub zone: Option<String>,
    stopping_with_guests_kept: AtomicBool,
}

impl ExecHost {
    pub async fn new(config: &WorkerConfig, worker_id: String) -> Result<Arc<ExecHost>> {
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
            stopping_with_guests_kept: AtomicBool::new(false),
        }))
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

    pub async fn stop_with_worker(&self) {
        match self.config.stop_guests_with_worker {
            true => self.pool.stop_all_units().await,
            false => self.pool.stop_boots().await,
        }
    }

    pub fn keep_guests_through_the_stop(&self) {
        self.stopping_with_guests_kept.store(true, Ordering::SeqCst);
    }

    pub fn leaves_guest_running(&self, abort: &AbortSignal) -> bool {
        !abort.was_requested() && self.stopping_with_guests_kept.load(Ordering::SeqCst)
    }
}
