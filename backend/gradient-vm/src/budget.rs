/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gradient_wire::types::{GuestCapacity, GuestSize, ResourceAmount};

#[derive(Clone)]
pub struct GuestBudget {
    total: GuestSize,
    held: Arc<Mutex<GuestSize>>,
}

impl GuestBudget {
    pub fn new(total: GuestSize) -> Self {
        Self {
            total,
            held: Arc::default(),
        }
    }

    pub fn total(&self) -> GuestSize {
        self.total
    }

    pub fn hold(&self, size: GuestSize) -> Option<BudgetHold> {
        let mut held = lock(&self.held);
        let wanted = add(*held, size);
        if !within(wanted, self.total) {
            return None;
        }
        *held = wanted;
        Some(self.hold_of(size))
    }

    pub fn adopt(&self, size: GuestSize) -> BudgetHold {
        let mut held = lock(&self.held);
        *held = add(*held, size);
        self.hold_of(size)
    }

    pub fn capacity(&self) -> GuestCapacity {
        let held = *lock(&self.held);
        let amount = |total: u64, held: u64| ResourceAmount {
            total,
            free: total.saturating_sub(held),
        };
        GuestCapacity {
            cpus: amount(u64::from(self.total.cpus), u64::from(held.cpus)),
            memory_mb: amount(self.total.memory_mb, held.memory_mb),
            disk_mb: amount(self.total.disk_mb, held.disk_mb),
        }
    }

    fn hold_of(&self, size: GuestSize) -> BudgetHold {
        BudgetHold {
            size,
            budget: self.clone(),
        }
    }
}

pub struct BudgetHold {
    size: GuestSize,
    budget: GuestBudget,
}

impl BudgetHold {
    pub fn size(&self) -> GuestSize {
        self.size
    }

    pub fn resize(&mut self, size: GuestSize) -> bool {
        let mut held = lock(&self.budget.held);
        let wanted = add(subtract(*held, self.size), size);
        if !within(wanted, self.budget.total) {
            return false;
        }
        *held = wanted;
        self.size = size;
        true
    }
}

impl Drop for BudgetHold {
    fn drop(&mut self) {
        let mut held = lock(&self.budget.held);
        *held = subtract(*held, self.size);
    }
}

fn add(a: GuestSize, b: GuestSize) -> GuestSize {
    GuestSize {
        cpus: a.cpus.saturating_add(b.cpus),
        memory_mb: a.memory_mb.saturating_add(b.memory_mb),
        disk_mb: a.disk_mb.saturating_add(b.disk_mb),
    }
}

fn subtract(a: GuestSize, b: GuestSize) -> GuestSize {
    GuestSize {
        cpus: a.cpus.saturating_sub(b.cpus),
        memory_mb: a.memory_mb.saturating_sub(b.memory_mb),
        disk_mb: a.disk_mb.saturating_sub(b.disk_mb),
    }
}

fn within(size: GuestSize, total: GuestSize) -> bool {
    size.cpus <= total.cpus && size.memory_mb <= total.memory_mb && size.disk_mb <= total.disk_mb
}

fn lock(held: &Mutex<GuestSize>) -> MutexGuard<'_, GuestSize> {
    held.lock().unwrap_or_else(PoisonError::into_inner)
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
    fn a_hold_beyond_the_total_is_refused_and_a_drop_frees_it() {
        let budget = GuestBudget::new(size(4, 1000, 20_000));
        let first = budget.hold(size(2, 500, 10_240)).unwrap();
        let second = budget.hold(size(2, 500, 9_000)).unwrap();
        assert!(budget.hold(size(1, 0, 0)).is_none(), "every CPU is held");
        assert_eq!(
            budget.capacity().memory_mb,
            ResourceAmount {
                total: 1000,
                free: 0
            }
        );
        drop(first);
        assert_eq!(budget.capacity().cpus, ResourceAmount { total: 4, free: 2 });
        assert!(budget.hold(size(2, 500, 10_240)).is_some());
        drop(second);
        let adopted = budget.adopt(size(8, 4000, 0));
        assert_eq!(
            budget.capacity().cpus.free,
            0,
            "an adopted guest may go over"
        );
        drop(adopted);
        assert_eq!(budget.capacity().cpus.free, 4);
    }
}
