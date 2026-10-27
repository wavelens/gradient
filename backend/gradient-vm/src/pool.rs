/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio_util::task::TaskTracker;
use tracing::{info, warn};

use crate::agent::GuestBootError;
use crate::config::VmConfig;
use crate::guest::Guest;
use crate::hypervisor::GuestImage;
use crate::state::read_states;
use crate::unit::GuestUnits;

const FIRST_CID: u32 = 3;
const BOOT_RETRY: Duration = Duration::from_secs(30);

pub struct GuestPool {
    config: VmConfig,
    units: GuestUnits,
    warm: Mutex<Vec<Guest>>,
    booting: AtomicU32,
    upcoming_exec_jobs: AtomicU32,
    draining: AtomicBool,
    reserved_cids: Mutex<BTreeSet<u32>>,
    tasks: TaskTracker,
}

impl GuestPool {
    pub fn new(config: VmConfig, units: GuestUnits) -> Arc<Self> {
        Arc::new(Self {
            config,
            units,
            warm: Mutex::default(),
            booting: AtomicU32::new(0),
            upcoming_exec_jobs: AtomicU32::new(0),
            draining: AtomicBool::new(false),
            reserved_cids: Mutex::default(),
            tasks: TaskTracker::new(),
        })
    }

    pub fn config(&self) -> &VmConfig {
        &self.config
    }

    pub fn units(&self) -> &GuestUnits {
        &self.units
    }

    pub async fn take(self: &Arc<Self>) -> Result<Guest, GuestBootError> {
        let warm = {
            let mut warm = lock(&self.warm);
            (!warm.is_empty()).then(|| warm.remove(0))
        };
        let guest = match warm {
            Some(guest) => Ok(guest),
            None => self.boot_one().await,
        };
        self.refill();
        guest
    }

    pub fn refill(self: &Arc<Self>) {
        let wanted = wanted_boots(
            self.warm_count(),
            self.booting.load(Ordering::SeqCst),
            self.target(),
            self.draining.load(Ordering::SeqCst),
        );
        for _ in 0..wanted {
            self.booting.fetch_add(1, Ordering::SeqCst);
            let pool = Arc::clone(self);
            self.tasks.spawn(async move { pool.boot_into_pool().await });
        }
    }

    pub fn set_upcoming_exec_jobs(self: &Arc<Self>, count: u32) {
        let before = self.target();
        self.upcoming_exec_jobs.store(count, Ordering::SeqCst);
        let target = self.target();
        if target != before {
            info!(target, "warm guest target changed");
        }
        self.refill();
        self.trim();
    }

    pub fn trim(self: &Arc<Self>) {
        let doomed = {
            let mut warm = lock(&self.warm);
            let surplus = surplus(count(warm.len()), self.target()) as usize;
            let keep = warm.len().saturating_sub(surplus);
            warm.split_off(keep)
        };
        for guest in doomed {
            let pool = Arc::clone(self);
            self.tasks
                .spawn(async move { guest.teardown(&pool.units).await });
        }
    }

    pub fn target(&self) -> u32 {
        warm_target(
            self.config.pre_warm,
            self.config.max_pre_warm,
            self.upcoming_exec_jobs.load(Ordering::SeqCst),
        )
    }

    pub fn set_draining(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    pub fn warm_count(&self) -> u32 {
        count(lock(&self.warm).len())
    }

    pub async fn stop_warm(&self) {
        let warm = std::mem::take(&mut *lock(&self.warm));
        for guest in warm {
            guest.teardown(&self.units).await;
        }
    }

    pub async fn stop_all_units(&self) {
        self.set_draining();
        lock(&self.warm).clear();
        match self.units.list().await {
            Ok(units) => {
                for (id, _) in units {
                    if let Err(e) = self.units.stop(&id).await {
                        warn!(guest = %id, error = %e, "could not stop a guest unit");
                    }
                }
            }
            Err(e) => warn!(error = %e, "could not list guest units"),
        }
        for (id, state) in read_states(&self.config.guests_dir) {
            let per_job_dirs = state.map(|state| state.per_job_dirs).unwrap_or_default();
            let dir = self.config.guests_dir.join(id.to_string());
            for dir in per_job_dirs.iter().chain([&dir]) {
                if let Err(e) = std::fs::remove_dir_all(dir) {
                    warn!(dir = %dir.display(), error = %e, "could not remove a guest directory");
                }
            }
        }
    }

    pub fn next_cid(&self) -> u32 {
        let mut reserved = lock(&self.reserved_cids);
        let on_disk: BTreeSet<u32> = read_states(&self.config.guests_dir)
            .into_iter()
            .filter_map(|(_, state)| state.ok().map(|state| state.cid))
            .collect();
        let cid = (FIRST_CID..=u32::MAX)
            .find(|cid| !reserved.contains(cid) && !on_disk.contains(cid))
            .unwrap_or(FIRST_CID);
        reserved.insert(cid);
        cid
    }

    async fn boot_one(&self) -> Result<Guest, GuestBootError> {
        let image =
            GuestImage::load(&self.config.image).map_err(|e| GuestBootError(format!("{e:#}")))?;
        let cid = self.next_cid();
        let guest = Guest::boot(&self.units, &self.config, &image, cid).await;
        lock(&self.reserved_cids).remove(&cid);
        guest
    }

    async fn boot_into_pool(self: Arc<Self>) {
        loop {
            match self.boot_one().await {
                Ok(guest) => return self.keep_or_stop(guest).await,
                Err(e) => warn!(error = %e, "warm guest boot failed, retrying in 30s"),
            }
            tokio::time::sleep(BOOT_RETRY).await;
            let booting = self.booting.load(Ordering::SeqCst);
            let still_wanted = !self.draining.load(Ordering::SeqCst)
                && self.warm_count() + booting <= self.target();
            if !still_wanted {
                self.booting.fetch_sub(1, Ordering::SeqCst);
                return;
            }
        }
    }

    async fn keep_or_stop(&self, guest: Guest) {
        let surplus_guest = {
            let mut warm = lock(&self.warm);
            self.booting.fetch_sub(1, Ordering::SeqCst);
            if self.draining.load(Ordering::SeqCst) || count(warm.len()) >= self.target() {
                Some(guest)
            } else {
                warm.push(guest);
                None
            }
        };
        if let Some(guest) = surplus_guest {
            guest.teardown(&self.units).await;
        }
    }
}

pub fn warm_target(pre_warm: u32, max_pre_warm: u32, upcoming_exec_jobs: u32) -> u32 {
    pre_warm.max(upcoming_exec_jobs.min(max_pre_warm))
}

pub fn wanted_boots(warm: u32, booting: u32, target: u32, draining: bool) -> u32 {
    if draining {
        return 0;
    }
    target.saturating_sub(warm.saturating_add(booting))
}

pub fn surplus(warm: u32, target: u32) -> u32 {
    warm.saturating_sub(target)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_boots_only_the_missing_guests() {
        assert_eq!(wanted_boots(1, 1, 3, false), 1);
        assert_eq!(wanted_boots(3, 0, 3, false), 0);
        assert_eq!(wanted_boots(0, 0, 3, true), 0);
    }

    #[test]
    fn the_warm_target_is_clamped_by_max_pre_warm() {
        assert_eq!(warm_target(1, 4, 10), 4);
        assert_eq!(warm_target(1, 0, 10), 1);
        assert_eq!(warm_target(3, 8, 2), 3);
        assert_eq!(warm_target(0, 8, 5), 5);
        assert_eq!(warm_target(2, 8, 0), 2);
    }

    #[test]
    fn warm_guests_above_a_lower_target_are_stopped() {
        assert_eq!(surplus(5, 2), 3);
        assert_eq!(surplus(1, 2), 0);
        assert_eq!(surplus(2, 2), 0);
    }
}
