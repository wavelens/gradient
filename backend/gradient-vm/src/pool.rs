/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
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
    slots: Mutex<Slots<Guest>>,
    upcoming_exec_jobs: AtomicU32,
    draining: AtomicBool,
    reserved_cids: Mutex<BTreeSet<u32>>,
    stopping: CancellationToken,
    tasks: TaskTracker,
}

impl GuestPool {
    pub fn new(config: VmConfig, units: GuestUnits) -> Arc<Self> {
        Arc::new(Self {
            config,
            units,
            slots: Mutex::new(Slots::new()),
            upcoming_exec_jobs: AtomicU32::new(0),
            draining: AtomicBool::new(false),
            reserved_cids: Mutex::default(),
            stopping: CancellationToken::new(),
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
        let warm = lock(&self.slots).take();
        let guest = match warm {
            Some(guest) => Ok(guest),
            None => self.cold_boot().await,
        };
        self.refill();
        guest
    }

    async fn cold_boot(self: &Arc<Self>) -> Result<Guest, GuestBootError> {
        let (sender, receiver) = oneshot::channel();
        let pool = Arc::clone(self);
        self.tasks.spawn(async move {
            if let Err(Ok(guest)) = sender.send(pool.boot_one().await) {
                pool.keep_or_stop(guest).await;
            }
        });
        receiver
            .await
            .unwrap_or_else(|_| Err(GuestBootError("the guest pool stopped".to_owned())))
    }

    pub fn refill(self: &Arc<Self>) {
        let wanted = lock(&self.slots).start_boots(self.target(), self.draining());
        for _ in 0..wanted {
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
        let doomed = lock(&self.slots).surplus(self.target());
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
        count(lock(&self.slots).warm.len())
    }

    pub async fn stop_warm(&self) {
        self.stop_boots().await;
        let warm = std::mem::take(&mut lock(&self.slots).warm);
        for guest in warm {
            guest.teardown(&self.units).await;
        }
    }

    pub async fn stop_all_units(&self) {
        self.stop_boots().await;
        lock(&self.slots).warm.clear();
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
        let guest = Guest::boot(&self.units, &self.config, &image, cid, &self.stopping).await;
        lock(&self.reserved_cids).remove(&cid);
        guest
    }

    async fn boot_into_pool(self: Arc<Self>) {
        loop {
            match self.boot_one().await {
                Ok(guest) => {
                    let surplus_guest =
                        lock(&self.slots).finish_boot(guest, self.target(), self.draining());
                    return self.stop_surplus(surplus_guest).await;
                }
                Err(e) => warn!(error = %e, "warm guest boot failed, retrying in 30s"),
            }
            tokio::select! {
                () = tokio::time::sleep(BOOT_RETRY) => {}
                () = self.stopping.cancelled() => {}
            }
            if !lock(&self.slots).retry_boot(self.target(), self.draining()) {
                return;
            }
        }
    }

    async fn keep_or_stop(&self, guest: Guest) {
        let surplus_guest = lock(&self.slots).offer(guest, self.target(), self.draining());
        self.stop_surplus(surplus_guest).await;
    }

    async fn stop_surplus(&self, guest: Option<Guest>) {
        if let Some(guest) = guest {
            guest.teardown(&self.units).await;
        }
    }

    async fn stop_boots(&self) {
        self.set_draining();
        self.stopping.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }

    fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }
}

struct Slots<T> {
    warm: Vec<T>,
    booting: u32,
}

impl<T> Slots<T> {
    fn new() -> Self {
        Self {
            warm: Vec::new(),
            booting: 0,
        }
    }

    fn take(&mut self) -> Option<T> {
        (!self.warm.is_empty()).then(|| self.warm.remove(0))
    }

    fn start_boots(&mut self, target: u32, draining: bool) -> u32 {
        let wanted = wanted_boots(count(self.warm.len()), self.booting, target, draining);
        self.booting += wanted;
        wanted
    }

    fn finish_boot(&mut self, guest: T, target: u32, draining: bool) -> Option<T> {
        self.booting = self.booting.saturating_sub(1);
        self.offer(guest, target, draining)
    }

    fn offer(&mut self, guest: T, target: u32, draining: bool) -> Option<T> {
        if draining || count(self.warm.len()) >= target {
            return Some(guest);
        }
        self.warm.push(guest);
        None
    }

    fn retry_boot(&mut self, target: u32, draining: bool) -> bool {
        let wanted = !draining && count(self.warm.len()) + self.booting <= target;
        if !wanted {
            self.booting = self.booting.saturating_sub(1);
        }
        wanted
    }

    fn surplus(&mut self, target: u32) -> Vec<T> {
        let surplus = surplus(count(self.warm.len()), target) as usize;
        let keep = self.warm.len().saturating_sub(surplus);
        self.warm.split_off(keep)
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
    fn a_second_refill_counts_the_boots_of_the_first() {
        let mut slots = Slots::<u32>::new();
        assert_eq!(slots.start_boots(3, false), 3);
        assert_eq!(slots.start_boots(3, false), 0);
        assert_eq!(slots.finish_boot(7, 3, false), None);
        assert_eq!(slots.start_boots(3, false), 0);
        assert_eq!(slots.take(), Some(7));
        assert_eq!(slots.start_boots(3, false), 1);
    }

    #[test]
    fn a_boot_finishing_at_a_full_pool_is_handed_back() {
        let mut slots = Slots::<u32>::new();
        assert_eq!(slots.start_boots(1, false), 1);
        assert_eq!(slots.finish_boot(1, 1, false), None);
        assert_eq!(slots.finish_boot(2, 1, false), Some(2));
        assert_eq!(slots.booting, 0);
    }

    #[test]
    fn a_failed_boot_stops_retrying_once_the_target_drops() {
        let mut slots = Slots::<u32>::new();
        assert_eq!(slots.start_boots(2, false), 2);
        assert!(slots.retry_boot(2, false));
        assert!(!slots.retry_boot(1, false));
        assert_eq!(slots.booting, 1);
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
