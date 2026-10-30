/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use gradient_wire::types::{
    GuestCapacity, GuestSize, ResourceAmount, WarmGuests, count_warm_guests,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};

use crate::agent::GuestBootError;
use crate::budget::{BudgetHold, GuestBudget};
use crate::config::VmConfig;
use crate::custom_image::custom_boot_error;
use crate::guest::{Guest, remove_guest_dirs};
use crate::hypervisor::GuestImage;
use crate::reattach::adopt_size;
use crate::state::{GuestId, GuestState, read_states};
use crate::unit::GuestUnits;
use crate::vm_states::VmStates;

pub const NO_GUEST_BUDGET: &str = "the worker has no guest budget left for this job";

const FIRST_CID: u32 = 3;
const BOOT_RETRY: Duration = Duration::from_secs(30);

type Taken = Result<(Guest, BudgetHold), GuestBootError>;

type SizeKey = (u32, u64);

struct WarmGuest {
    guest: Guest,
    hold: BudgetHold,
}

struct WarmBootTicket {
    id: u64,
    stop: CancellationToken,
    size: GuestSize,
}

struct WarmBoots<H> {
    next: u64,
    reserved: BTreeMap<u64, (CancellationToken, H)>,
}

impl<H> WarmBoots<H> {
    fn new() -> Self {
        Self {
            next: 0,
            reserved: BTreeMap::new(),
        }
    }

    fn reserve(
        &mut self,
        hold: H,
        size: GuestSize,
        pool_stop: &CancellationToken,
    ) -> WarmBootTicket {
        let (id, stop) = (self.next, pool_stop.child_token());
        self.next += 1;
        self.reserved.insert(id, (stop.clone(), hold));
        WarmBootTicket { id, stop, size }
    }

    fn finish(&mut self, id: u64) -> Option<H> {
        self.reserved.remove(&id).map(|(_, hold)| hold)
    }

    fn holds(&self) -> impl Iterator<Item = &H> {
        self.reserved.values().map(|(_, hold)| hold)
    }

    fn steal(&mut self) -> Option<H> {
        let (stop, hold) = self.reserved.pop_last()?.1;
        stop.cancel();
        Some(hold)
    }
}

pub struct GuestPool {
    config: VmConfig,
    units: GuestUnits,
    budget: GuestBudget,
    slots: Mutex<BTreeMap<SizeKey, Slots<WarmGuest>>>,
    warm_boots: Mutex<WarmBoots<BudgetHold>>,
    wanted: Mutex<Vec<WarmGuests>>,
    draining: AtomicBool,
    reserved_cids: Mutex<BTreeSet<u32>>,
    stopping: CancellationToken,
    tasks: TaskTracker,
    states: VmStates,
}

impl GuestPool {
    pub fn new(config: VmConfig, units: GuestUnits, budget: GuestBudget) -> Arc<Self> {
        Arc::new(Self {
            config,
            units,
            budget,
            slots: Mutex::default(),
            warm_boots: Mutex::new(WarmBoots::new()),
            wanted: Mutex::default(),
            draining: AtomicBool::new(false),
            reserved_cids: Mutex::default(),
            stopping: CancellationToken::new(),
            tasks: TaskTracker::new(),
            states: VmStates::default(),
        })
    }

    pub fn config(&self) -> &VmConfig {
        &self.config
    }

    pub fn units(&self) -> &GuestUnits {
        &self.units
    }

    pub fn states(&self) -> &VmStates {
        &self.states
    }

    pub fn capacity(&self) -> GuestCapacity {
        let mut warm: Vec<GuestSize> = lock(&self.slots)
            .values()
            .flat_map(|slots| slots.warm.iter().map(|warm| warm.hold.size()))
            .collect();
        warm.extend(lock(&self.warm_boots).holds().map(BudgetHold::size));
        with_warm_guests_free(self.budget.capacity(), &warm)
    }

    pub fn adopt_size(&self, state: &GuestState) -> BudgetHold {
        adopt_size(&self.budget, state, self.warm_size().unwrap_or_default())
    }

    pub async fn take(self: &Arc<Self>, job_id: String, size: GuestSize) -> Taken {
        let image = self.worker_image()?;
        let size = job_size(size, &image);
        let warm = lock(&self.slots)
            .get_mut(&size_key(size))
            .and_then(|slots| slots.take(|warm| warm.hold.size().warm_guest_takes(size)));
        let taken = match warm {
            Some(warm) => self.claim_warm(warm, size).await,
            None => self.cold_boot(image.sized(&size), size).await,
        };
        self.refill();
        let (guest, hold) = taken?;
        self.running(guest, hold, size, job_id)
    }

    async fn claim_warm(&self, warm: WarmGuest, size: GuestSize) -> Taken {
        let WarmGuest { guest, mut hold } = warm;
        match self.make_room(|| hold.resize(size).then_some(())).await {
            Ok(()) => Ok((guest, hold)),
            Err(e) => {
                self.keep_or_stop(guest, hold).await;
                Err(e)
            }
        }
    }

    async fn cold_boot(self: &Arc<Self>, image: GuestImage, size: GuestSize) -> Taken {
        let hold = self.make_room(|| self.budget.hold(size)).await?;
        let (sender, receiver) = oneshot::channel();
        let pool = Arc::clone(self);
        self.tasks.spawn(async move {
            let booted = pool.boot_image(&image).await.map(|guest| (guest, hold));
            if let Err(Ok((guest, hold))) = sender.send(booted) {
                pool.keep_or_stop(guest, hold).await;
            }
        });
        receiver
            .await
            .unwrap_or_else(|_| Err(GuestBootError("the guest pool stopped".to_owned())))
    }

    async fn make_room<T>(
        &self,
        mut attempt: impl FnMut() -> Option<T>,
    ) -> Result<T, GuestBootError> {
        loop {
            if let Some(done) = attempt() {
                return Ok(done);
            }
            let evicted = lock(&self.slots)
                .values_mut()
                .find_map(|slots| slots.take(|_| true));
            if let Some(warm) = evicted {
                info!(guest = %warm.guest.id(), "stopping a warm guest to make room for a job");
                self.teardown_warm(warm).await;
                continue;
            }
            let Some(stolen) = lock(&self.warm_boots).steal() else {
                return Err(GuestBootError(NO_GUEST_BUDGET.to_owned()));
            };
            info!("stopping a warm guest boot to make room for a job");
            drop(stolen);
        }
    }

    fn running(
        self: &Arc<Self>,
        mut guest: Guest,
        hold: BudgetHold,
        size: GuestSize,
        job_id: String,
    ) -> Taken {
        if let Err(e) = guest.record_size(size) {
            self.retire(guest, hold);
            return Err(GuestBootError(format!("{e:#}")));
        }
        self.states.running(guest.id(), job_id);
        Ok((guest, hold))
    }

    pub fn refill(self: &Arc<Self>) {
        let draining = self.draining();
        for (key, target) in self.targets() {
            let wanted = slots_of(&mut lock(&self.slots), key).start_boots(target, draining);
            if wanted == 0 {
                continue;
            }
            let size = warm_size(key);
            let holds = warm_boot_holds(&self.budget, size, wanted);
            slots_of(&mut lock(&self.slots), key).cancel_boots(wanted - count(holds.len()));
            for hold in holds {
                let boot = lock(&self.warm_boots).reserve(hold, size, &self.stopping);
                let pool = Arc::clone(self);
                self.tasks
                    .spawn(async move { pool.boot_into_pool(boot).await });
            }
        }
    }

    pub fn adopt_warm(self: &Arc<Self>, guest: Guest) {
        let hold = self.adopt_size(guest.state());
        let key = size_key(hold.size());
        let (target, draining) = (self.target_of(key), self.draining());
        let surplus = self.park(key, WarmGuest { guest, hold }, |slots, warm| {
            slots.offer(warm, target, draining)
        });
        if let Some(warm) = surplus {
            self.retire(warm.guest, warm.hold);
        }
    }

    pub fn retire<Held: Send + 'static>(self: &Arc<Self>, guest: Guest, held: Held) {
        let pool = Arc::clone(self);
        self.tasks.spawn(async move {
            pool.teardown(guest).await;
            drop(held);
            pool.refill();
        });
    }

    pub fn stop_warm_in_background(self: &Arc<Self>) {
        let pool = Arc::clone(self);
        self.tasks.spawn(async move { pool.stop_warm().await });
    }

    pub fn want_warm_guests(self: &Arc<Self>, wanted: Vec<WarmGuests>) {
        let before = self.targets();
        *lock(&self.wanted) = wanted;
        let targets = self.targets();
        if targets != before {
            let target: u32 = targets.values().sum();
            info!(target, ?targets, "warm guest target changed");
        }
        self.refill();
        self.trim();
    }

    pub fn trim(self: &Arc<Self>) {
        let targets = self.targets();
        let doomed: Vec<WarmGuest> = lock(&self.slots)
            .iter_mut()
            .flat_map(|(key, slots)| slots.surplus(targets.get(key).copied().unwrap_or(0)))
            .collect();
        for warm in doomed {
            let pool = Arc::clone(self);
            self.tasks
                .spawn(async move { pool.teardown_warm(warm).await });
        }
    }

    fn targets(&self) -> BTreeMap<SizeKey, u32> {
        warm_targets(
            self.config.pre_warm,
            self.config.max_pre_warm,
            self.warm_size().ok(),
            &lock(&self.wanted),
        )
    }

    fn target_of(&self, key: SizeKey) -> u32 {
        self.targets().get(&key).copied().unwrap_or(0)
    }

    pub fn set_draining(&self) {
        self.draining.store(true, Ordering::SeqCst);
    }

    pub fn reopen(self: &Arc<Self>) {
        self.draining.store(false, Ordering::SeqCst);
        self.refill();
    }

    pub fn warm_guests(&self) -> Vec<WarmGuests> {
        count_warm_guests(
            lock(&self.slots)
                .iter()
                .map(|(key, slots)| (warm_size(*key), count(slots.warm.len()))),
        )
    }

    pub async fn stop_warm(&self) {
        self.set_draining();
        let warm: Vec<WarmGuest> = lock(&self.slots)
            .values_mut()
            .flat_map(|slots| std::mem::take(&mut slots.warm))
            .collect();
        for warm in warm {
            self.teardown_warm(warm).await;
        }
    }

    pub async fn stop_all_units(&self) {
        self.stop_boots().await;
        lock(&self.slots).clear();
        self.stop_every_unit().await;
    }

    pub async fn stop_every_unit(&self) {
        let still_running = match self.units.list().await {
            Ok(units) => self.stop_units(units).await,
            Err(e) => {
                warn!(error = %e, "could not list guest units");
                return;
            }
        };
        for (id, state) in read_states(&self.config.guests_dir) {
            let per_job_dirs = state.map(|state| state.per_job_dirs).unwrap_or_default();
            let dir = self.config.guests_dir.join(id.to_string());
            remove_guest_dirs(&dir, &per_job_dirs, !still_running.contains(&id));
        }
    }

    async fn stop_units(&self, units: Vec<(GuestId, bool)>) -> BTreeSet<GuestId> {
        let mut still_running = BTreeSet::new();
        for (id, _) in units {
            if let Err(e) = self.units.stop(&id).await {
                warn!(guest = %id, error = %e, "could not stop a guest unit");
                still_running.insert(id);
            }
        }
        still_running
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

    pub async fn boot_custom(
        self: &Arc<Self>,
        custom: &Path,
        job_id: String,
        size: GuestSize,
    ) -> Taken {
        let size = job_size(size, &self.worker_image()?);
        let image = load_image(custom)?.sized(&size);
        let hold = self.make_room(|| self.budget.hold(size)).await?;
        let guest = self
            .boot_image(&image)
            .await
            .map_err(|e| custom_boot_error(e, custom))?;
        self.running(guest, hold, size, job_id)
    }

    async fn boot_warm(
        &self,
        stop: &CancellationToken,
        size: GuestSize,
    ) -> Result<Guest, GuestBootError> {
        self.boot_image_until(&self.worker_image()?.sized(&size), stop)
            .await
    }

    fn worker_image(&self) -> Result<GuestImage, GuestBootError> {
        load_image(&self.config.image)
    }

    fn warm_size(&self) -> Result<GuestSize, GuestBootError> {
        self.worker_image().map(|image| image.size())
    }

    async fn boot_image(&self, image: &GuestImage) -> Result<Guest, GuestBootError> {
        self.boot_image_until(image, &self.stopping).await
    }

    async fn boot_image_until(
        &self,
        image: &GuestImage,
        stop: &CancellationToken,
    ) -> Result<Guest, GuestBootError> {
        let cid = self.next_cid();
        let guest = Guest::boot(&self.units, &self.config, image, cid, stop).await;
        lock(&self.reserved_cids).remove(&cid);
        guest
    }

    async fn boot_into_pool(self: Arc<Self>, boot: WarmBootTicket) {
        let key = size_key(boot.size);
        loop {
            match self.boot_warm(&boot.stop, boot.size).await {
                Ok(guest) => return self.park_warm_boot(&boot, guest).await,
                Err(e) if !boot.stop.is_cancelled() => {
                    warn!(error = %e, "warm guest boot failed, retrying in 30s");
                }
                Err(_) => {}
            }
            tokio::select! {
                () = tokio::time::sleep(BOOT_RETRY) => {}
                () = boot.stop.cancelled() => {}
            }
            let (target, draining) = (self.target_of(key), self.draining());
            let retry = !boot.stop.is_cancelled()
                && slots_of(&mut lock(&self.slots), key).retry_boot(target, draining);
            if !retry {
                if boot.stop.is_cancelled() {
                    slots_of(&mut lock(&self.slots), key).cancel_boots(1);
                }
                drop(lock(&self.warm_boots).finish(boot.id));
                return;
            }
        }
    }

    async fn park_warm_boot(&self, boot: &WarmBootTicket, mut guest: Guest) {
        let key = size_key(boot.size);
        let hold = lock(&self.warm_boots).finish(boot.id);
        let recorded = guest.record_size(boot.size);
        let Some(hold) = hold.filter(|_| recorded.is_ok()) else {
            if let Err(e) = recorded {
                warn!(error = %format!("{e:#}"), "warm guest size not recorded");
            }
            slots_of(&mut lock(&self.slots), key).cancel_boots(1);
            return self.teardown(guest).await;
        };
        let (target, draining) = (self.target_of(key), self.draining());
        let surplus = self.park(key, WarmGuest { guest, hold }, |slots, warm| {
            slots.finish_boot(warm, target, draining)
        });
        self.stop_surplus(surplus).await;
    }

    async fn keep_or_stop(&self, mut guest: Guest, mut hold: BudgetHold) {
        let key = size_key(hold.size());
        let target = self.target_of(key);
        let size = warm_size(key);
        let fits_warm = target > 0 && hold.resize(size) && guest.record_size(size).is_ok();
        let warm = WarmGuest { guest, hold };
        if !fits_warm {
            return self.teardown_warm(warm).await;
        }
        let draining = self.draining();
        let surplus = self.park(key, warm, |slots, warm| slots.offer(warm, target, draining));
        self.stop_surplus(surplus).await;
    }

    async fn stop_surplus(&self, warm: Option<WarmGuest>) {
        if let Some(warm) = warm {
            self.teardown_warm(warm).await;
        }
    }

    fn park(
        &self,
        key: SizeKey,
        warm: WarmGuest,
        place: impl FnOnce(&mut Slots<WarmGuest>, WarmGuest) -> Option<WarmGuest>,
    ) -> Option<WarmGuest> {
        let id = warm.guest.id().clone();
        let surplus = place(slots_of(&mut lock(&self.slots), key), warm);
        if surplus.is_none() {
            self.states.warm(&id);
        }
        surplus
    }

    async fn teardown_warm(&self, warm: WarmGuest) {
        self.teardown(warm.guest).await;
        drop(warm.hold);
    }

    async fn teardown(&self, guest: Guest) {
        self.states.stopped(guest.id());
        guest.teardown(&self.units).await;
    }

    pub async fn stop_boots(&self) {
        self.set_draining();
        self.stopping.cancel();
        self.tasks.close();
        self.tasks.wait().await;
    }

    fn draining(&self) -> bool {
        self.draining.load(Ordering::SeqCst)
    }
}

fn load_image(image: &Path) -> Result<GuestImage, GuestBootError> {
    GuestImage::load(image).map_err(|e| GuestBootError(format!("{e:#}")))
}

fn job_size(size: GuestSize, image: &GuestImage) -> GuestSize {
    match size.is_unset() {
        true => image.size(),
        false => size,
    }
}

fn warm_boot_holds(budget: &GuestBudget, size: GuestSize, wanted: u32) -> Vec<BudgetHold> {
    (0..wanted).map_while(|_| budget.hold(size)).collect()
}

fn with_warm_guests_free(capacity: GuestCapacity, warm: &[GuestSize]) -> GuestCapacity {
    let freed = |amount: ResourceAmount, held: u64| ResourceAmount {
        total: amount.total,
        free: amount.free.saturating_add(held).min(amount.total),
    };
    let sum = |of: fn(&GuestSize) -> u64| warm.iter().map(of).sum::<u64>();
    GuestCapacity {
        cpus: freed(capacity.cpus, sum(|size| u64::from(size.cpus))),
        memory_mb: freed(capacity.memory_mb, sum(|size| size.memory_mb)),
        disk_mb: freed(capacity.disk_mb, sum(|size| size.disk_mb)),
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

    fn take(&mut self, wanted: impl Fn(&T) -> bool) -> Option<T> {
        let index = self.warm.iter().position(wanted)?;
        Some(self.warm.remove(index))
    }

    fn start_boots(&mut self, target: u32, draining: bool) -> u32 {
        let wanted = wanted_boots(count(self.warm.len()), self.booting, target, draining);
        self.booting += wanted;
        wanted
    }

    fn cancel_boots(&mut self, boots: u32) {
        self.booting = self.booting.saturating_sub(boots);
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

pub fn wanted_warm_guests(count: u32, sizes: Vec<WarmGuests>) -> Vec<WarmGuests> {
    match sizes.is_empty() {
        true => vec![WarmGuests {
            size: GuestSize::default(),
            count,
        }],
        false => sizes,
    }
}

fn warm_targets(
    pre_warm: u32,
    max_pre_warm: u32,
    image_size: Option<GuestSize>,
    wanted: &[WarmGuests],
) -> BTreeMap<SizeKey, u32> {
    let Some(image) = image_size.map(size_key) else {
        return BTreeMap::new();
    };
    let asked = asked_beyond_pre_warm(image, pre_warm, wanted);
    let mut targets = share_out(max_pre_warm.saturating_sub(pre_warm), &asked);
    if pre_warm > 0 {
        *targets.entry(image).or_default() += pre_warm;
    }
    targets
}

fn asked_beyond_pre_warm(
    image: SizeKey,
    pre_warm: u32,
    wanted: &[WarmGuests],
) -> Vec<(SizeKey, u32)> {
    let mut asked: BTreeMap<SizeKey, u32> = BTreeMap::new();
    for warm in wanted {
        let key = match warm.size.is_unset() {
            true => image,
            false => size_key(warm.size),
        };
        *asked.entry(key).or_default() += warm.count;
    }
    if let Some(at_image) = asked.get_mut(&image) {
        *at_image = at_image.saturating_sub(pre_warm);
    }
    let mut asked: Vec<(SizeKey, u32)> = asked.into_iter().filter(|(_, n)| *n > 0).collect();
    asked.sort_by_key(|(key, n)| (Reverse(*n), *key != image, *key));
    asked
}

fn share_out(max_pre_warm: u32, asked: &[(SizeKey, u32)]) -> BTreeMap<SizeKey, u32> {
    let mut targets = BTreeMap::new();
    let mut left = max_pre_warm;
    for round in 0.. {
        let open: Vec<SizeKey> = asked
            .iter()
            .filter(|(_, n)| *n > round)
            .map(|(key, _)| *key)
            .collect();
        if open.is_empty() || left == 0 {
            break;
        }
        for key in open
            .into_iter()
            .take(usize::try_from(left).unwrap_or(usize::MAX))
        {
            *targets.entry(key).or_default() += 1;
            left -= 1;
        }
    }
    targets
}

fn size_key(size: GuestSize) -> SizeKey {
    (size.cpus, size.memory_mb)
}

fn warm_size((cpus, memory_mb): SizeKey) -> GuestSize {
    GuestSize {
        cpus,
        memory_mb,
        disk_mb: 0,
    }
}

fn slots_of<T>(slots: &mut BTreeMap<SizeKey, Slots<T>>, key: SizeKey) -> &mut Slots<T> {
    slots.entry(key).or_insert_with(Slots::new)
}

fn wanted_boots(warm: u32, booting: u32, target: u32, draining: bool) -> u32 {
    if draining {
        return 0;
    }
    target.saturating_sub(warm.saturating_add(booting))
}

fn surplus(warm: u32, target: u32) -> u32 {
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

    fn size(cpus: u32, memory_mb: u64, disk_mb: u64) -> GuestSize {
        GuestSize {
            cpus,
            memory_mb,
            disk_mb,
        }
    }

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
        assert_eq!(slots.take(|_| true), Some(7));
        assert_eq!(slots.start_boots(3, false), 1);
    }

    #[test]
    fn a_warm_guest_serves_only_its_own_size() {
        let warm = size(2, 500, 0);
        let mut slots = Slots::<GuestSize>::new();
        assert_eq!(slots.offer(warm, 1, false), None);
        assert_eq!(
            slots.take(|warm| warm.warm_guest_takes(size(4, 768, 256))),
            None
        );
        assert_eq!(
            slots.take(|warm| warm.warm_guest_takes(size(2, 500, 10_240))),
            Some(warm)
        );
    }

    #[test]
    fn the_pool_boots_no_warm_guest_beyond_the_budget() {
        let image = size(2, 500, 0);
        let budget = GuestBudget::new(size(2, 500, 10_240));
        let mut slots = Slots::<BudgetHold>::new();
        let wanted = slots.start_boots(2, false);
        let mut holds = warm_boot_holds(&budget, image, wanted);
        assert_eq!(holds.len(), 1);
        slots.cancel_boots(wanted - count(holds.len()));
        assert_eq!(slots.booting, 1);
        let mut taken = holds.remove(0);
        assert!(
            taken.resize(GuestSize::default_request()),
            "the job adds its disk to the warm guest it takes"
        );
        assert!(warm_boot_holds(&budget, image, 1).is_empty());
    }

    #[test]
    fn a_job_takes_the_budget_of_a_booting_warm_guest() {
        let warm_size = size(2, 500, 0);
        let budget = GuestBudget::new(size(2, 500, 10_240));
        let pool_stop = CancellationToken::new();
        let mut boots = WarmBoots::new();
        let boot = boots.reserve(budget.hold(warm_size).unwrap(), warm_size, &pool_stop);
        assert_eq!(
            with_warm_guests_free(
                budget.capacity(),
                &boots.holds().map(BudgetHold::size).collect::<Vec<_>>()
            )
            .cpus
            .free,
            2,
            "a booting warm guest counts as free"
        );
        assert!(budget.hold(GuestSize::default_request()).is_none());

        drop(boots.steal().expect("the boot holds budget"));

        assert!(boot.stop.is_cancelled(), "the boot is stopped");
        assert!(budget.hold(GuestSize::default_request()).is_some());
        assert!(boots.finish(boot.id).is_none(), "the boot parks no guest");
        assert!(!pool_stop.is_cancelled());
    }

    #[test]
    fn warm_guests_count_as_free_for_a_job() {
        let budget = GuestBudget::new(size(4, 1000, 0));
        let _warm = budget.hold(size(2, 500, 0)).unwrap();
        let capacity = with_warm_guests_free(budget.capacity(), &[size(2, 500, 0)]);
        assert_eq!(capacity.cpus, ResourceAmount { total: 4, free: 4 });
        assert_eq!(capacity.memory_mb.free, 1000);
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

    fn wanted(sizes: &[(GuestSize, u32)]) -> Vec<WarmGuests> {
        sizes
            .iter()
            .map(|&(size, count)| WarmGuests { size, count })
            .collect()
    }

    #[test]
    fn the_image_size_target_is_clamped_by_max_pre_warm() {
        let image = size(2, 500, 0);
        let target = |pre_warm, max_pre_warm, upcoming| {
            let wanted = wanted_warm_guests(upcoming, Vec::new());
            warm_targets(pre_warm, max_pre_warm, Some(image), &wanted)
                .get(&size_key(image))
                .copied()
                .unwrap_or(0)
        };
        assert_eq!(target(1, 4, 10), 4);
        assert_eq!(target(1, 0, 10), 1);
        assert_eq!(target(3, 8, 2), 3);
        assert_eq!(target(0, 8, 5), 5);
        assert_eq!(target(2, 8, 0), 2);
        assert_eq!(target(0, 8, 0), 0);
    }

    #[test]
    fn every_asked_size_gets_a_warm_guest_before_any_gets_a_second() {
        let image = size(2, 500, 0);
        let large = size(16, 65_536, 0);
        let medium = size(8, 16_384, 0);
        let asked = wanted(&[(large, 5), (GuestSize::default(), 1), (medium, 1)]);

        assert_eq!(
            warm_targets(0, 3, Some(image), &asked),
            BTreeMap::from([
                (size_key(image), 1),
                (size_key(medium), 1),
                (size_key(large), 1)
            ])
        );
        assert_eq!(
            warm_targets(0, 2, Some(image), &asked),
            BTreeMap::from([(size_key(image), 1), (size_key(large), 1)]),
            "the most asked size goes first, then the image size"
        );
        assert_eq!(warm_targets(0, 5, Some(image), &asked)[&size_key(large)], 3);
    }

    #[test]
    fn pre_warm_guests_of_the_image_size_count_against_max_pre_warm() {
        let image = size(2, 500, 0);
        let large = size(16, 65_536, 0);
        assert_eq!(
            warm_targets(1, 3, Some(image), &wanted(&[(large, 4)])),
            BTreeMap::from([(size_key(image), 1), (size_key(large), 2)])
        );
        assert_eq!(
            warm_targets(1, 3, Some(image), &wanted(&[(image, 2), (large, 4)])),
            BTreeMap::from([(size_key(image), 2), (size_key(large), 1)]),
            "an image size job asks beyond the pre-warm guest"
        );
        assert_eq!(
            warm_targets(1, 3, None, &wanted(&[(GuestSize::default(), 4)])),
            BTreeMap::new(),
            "an unreadable image leaves the image size without a target"
        );
    }

    #[test]
    fn warm_guests_above_a_lower_target_are_stopped() {
        assert_eq!(surplus(5, 2), 3);
        assert_eq!(surplus(1, 2), 0);
        assert_eq!(surplus(2, 2), 0);
    }
}
