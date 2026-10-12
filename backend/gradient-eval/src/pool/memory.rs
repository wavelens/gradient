/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

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
}
