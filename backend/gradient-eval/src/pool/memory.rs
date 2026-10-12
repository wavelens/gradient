/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryGuard {
    None,
    MinFreeRam(u64),
    MaxTotal(u64),
}

impl MemoryGuard {
    pub fn is_active(&self) -> bool {
        !matches!(self, MemoryGuard::None)
    }
}

/// The 1 GiB is a ceiling, not a baseline: a fixed 1 GiB armed the guard on a 2 GiB host under
/// ordinary build load (#579). Depth enough to react before the kernel OOM killer is the whole
/// requirement.
pub fn memory_guard_bytes(min_free_ram_mb: u64, total_ram_bytes: u64) -> u64 {
    const MIN: u64 = 128 * 1024 * 1024;
    const MAX: u64 = 1024 * 1024 * 1024;
    if min_free_ram_mb > 0 {
        min_free_ram_mb * 1024 * 1024
    } else {
        (total_ram_bytes / 10).clamp(MIN, MAX)
    }
}

pub fn min_free_ram_guard(min_free_ram_mb: u64) -> MemoryGuard {
    MemoryGuard::MinFreeRam(memory_guard_bytes(min_free_ram_mb, total_memory_bytes()))
}

const TOTAL_RAM_FALLBACK_BYTES: u64 = 4 * 1024 * 1024 * 1024;

pub fn total_memory_bytes() -> u64 {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let total = sys.total_memory();
    if total == 0 {
        TOTAL_RAM_FALLBACK_BYTES
    } else {
        total
    }
}

pub(super) const REAPER_INTERVAL: Duration = Duration::from_millis(500);

pub(super) const REAP_COOLDOWN: Duration = Duration::from_secs(5);

/// Smaller victims leave the host under the margin and bring another reap (#579); only the
pub(super) fn victim(
    guard: &MemoryGuard,
    candidates: &[(u32, u64)],
    available: u64,
) -> Option<(u32, u64)> {
    let largest = |rss_min: u64| {
        candidates
            .iter()
            .copied()
            .filter(|&(_, rss)| rss >= rss_min)
            .max_by_key(|&(_, rss)| rss)
    };

    match guard {
        MemoryGuard::None => None,
        MemoryGuard::MinFreeRam(min_free_bytes) => {
            let shortfall = min_free_bytes.saturating_sub(available);
            (shortfall > 0).then(|| largest(shortfall)).flatten()
        }
        MemoryGuard::MaxTotal(max_total_bytes) => {
            let total: u64 = candidates.iter().map(|&(_, rss)| rss).sum();
            (total > *max_total_bytes).then(|| largest(0)).flatten()
        }
    }
}

pub(super) fn pressured(guard: &MemoryGuard, candidates: &[(u32, u64)], available: u64) -> bool {
    match guard {
        MemoryGuard::None => false,
        MemoryGuard::MinFreeRam(min_free_bytes) => available < *min_free_bytes,
        MemoryGuard::MaxTotal(max_total_bytes) => {
            candidates.iter().map(|&(_, rss)| rss).sum::<u64>() > *max_total_bytes
        }
    }
}

#[cfg(target_os = "linux")]
pub(super) fn rss_of_pid(pid: u32) -> Option<u64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    statm
        .split_whitespace()
        .nth(1)
        .and_then(|pages| pages.parse::<u64>().ok())
        .map(|pages| pages * 4096)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn rss_of_pid(_pid: u32) -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn memory_guard_bytes_honours_the_configured_value_and_adapts_otherwise() {
        assert_eq!(memory_guard_bytes(2048, 64 * GIB), 2048 * 1024 * 1024);
        assert_eq!(memory_guard_bytes(0, 4 * GIB), 4 * GIB / 10);
        assert_eq!(memory_guard_bytes(0, 64 * GIB), GIB);
        assert_eq!(memory_guard_bytes(0, 512 * MIB), 128 * MIB);
    }

    #[test]
    fn the_adaptive_margin_stays_a_small_share_of_the_host() {
        for total in [512 * MIB, GIB, 2 * GIB, 4 * GIB, 16 * GIB, 128 * GIB] {
            let margin = memory_guard_bytes(0, total);
            assert!(
                margin * 4 <= total,
                "margin {margin} is over a quarter of a {total}-byte host"
            );
        }
    }

    #[test]
    fn only_a_configured_guard_arms_the_reaper() {
        assert!(!MemoryGuard::None.is_active());
        assert!(MemoryGuard::MinFreeRam(1).is_active());
        assert!(MemoryGuard::MaxTotal(1).is_active());
    }

    #[test]
    fn a_configured_margin_is_taken_as_is_and_an_unset_one_adapts() {
        assert_eq!(min_free_ram_guard(512), MemoryGuard::MinFreeRam(512 * MIB));
        assert!(matches!(
            min_free_ram_guard(0),
            MemoryGuard::MinFreeRam(bytes) if bytes >= 128 * MIB
        ));
    }

    #[test]
    fn the_largest_sufficient_victim_is_reaped_and_the_rest_left_alone() {
        let available = 980 * MIB;
        let guard = MemoryGuard::MinFreeRam(GIB);
        assert_eq!(victim(&guard, &[(1442, 27 * MIB)], available), None);
        assert_eq!(
            victim(&guard, &[(1442, 44 * MIB)], available),
            Some((1442, 44 * MIB))
        );
        assert_eq!(victim(&guard, &[(1, 0), (2, 0), (3, 0)], available), None);
        assert_eq!(
            victim(
                &guard,
                &[(1, 100 * MIB), (2, 600 * MIB), (3, 300 * MIB)],
                900 * MIB
            ),
            Some((2, 600 * MIB))
        );
        assert_eq!(victim(&guard, &[(1, 8 * GIB)], 2 * GIB), None);
        assert_eq!(victim(&guard, &[], 0), None);
    }

    #[test]
    fn max_total_kills_the_largest_while_the_sum_is_over_the_cap() {
        let guard = MemoryGuard::MaxTotal(1024 * MIB);
        assert_eq!(
            victim(
                &guard,
                &[(1, 100 * MIB), (2, 600 * MIB), (3, 300 * MIB)],
                8 * GIB
            ),
            Some((2, 600 * MIB))
        );
    }

    #[test]
    fn max_total_keeps_every_subprocess_under_the_cap() {
        let guard = MemoryGuard::MaxTotal(1024 * MIB);
        assert_eq!(
            victim(&guard, &[(1, 100 * MIB), (2, 600 * MIB)], 8 * GIB),
            None
        );
    }

    #[test]
    fn no_guard_reaps_nothing() {
        assert_eq!(victim(&MemoryGuard::None, &[(1, 8 * GIB)], MIB), None);
    }

    #[test]
    fn pressure_is_judged_on_the_host_or_on_the_subprocesses() {
        let candidates = [(1, 600 * MIB), (2, 300 * MIB)];
        assert!(!pressured(&MemoryGuard::None, &candidates, MIB));
        assert!(pressured(
            &MemoryGuard::MinFreeRam(GIB),
            &candidates,
            512 * MIB
        ));
        assert!(!pressured(
            &MemoryGuard::MinFreeRam(GIB),
            &candidates,
            2 * GIB
        ));
        assert!(pressured(&MemoryGuard::MaxTotal(GIB), &candidates, 8 * GIB));
        assert!(!pressured(
            &MemoryGuard::MaxTotal(2 * GIB),
            &candidates,
            8 * GIB
        ));
    }
}
