/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::sync::Weak;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use super::pool::EvalWorkerPool;

const REAPER_INTERVAL: Duration = Duration::from_millis(500);

const REAP_COOLDOWN: Duration = Duration::from_secs(5);

/// The 1 GiB is a ceiling, not a floor.
/// As a floor it kept the guard armed on a 2 GiB host under ordinary build load (#579).
/// The margin only has to be deep enough to react before the kernel OOM killer.
pub fn memory_guard_bytes(min_free_ram_mb: u64, total_ram_bytes: u64) -> u64 {
    const MIN: u64 = 128 * 1024 * 1024;
    const MAX: u64 = 1024 * 1024 * 1024;
    if min_free_ram_mb > 0 {
        min_free_ram_mb * 1024 * 1024
    } else {
        (total_ram_bytes / 10).clamp(MIN, MAX)
    }
}

/// Only a victim with an RSS covering the entire shortfall is worth killing.
/// A smaller victim is leaving the host under the margin.
/// The next tick would then reap again (#579).
/// No kill can fix pressure that is not coming from evaluation.
pub(super) fn reap_victim(
    candidates: &[(u32, u64)],
    available: u64,
    min_free_bytes: u64,
) -> Option<(u32, u64)> {
    let shortfall = min_free_bytes.saturating_sub(available);
    if shortfall == 0 {
        return None;
    }

    candidates
        .iter()
        .copied()
        .filter(|&(_, rss)| rss >= shortfall)
        .max_by_key(|&(_, rss)| rss)
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

pub(super) async fn memory_reaper_loop(pool: Weak<EvalWorkerPool>, min_free_bytes: u64) {
    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let mut interval = tokio::time::interval(REAPER_INTERVAL);
    let mut last_reap: Option<Instant> = None;
    let mut reported_no_victim = false;
    loop {
        interval.tick().await;
        let Some(pool) = pool.upgrade() else {
            return;
        };

        if min_free_bytes == 0 {
            continue;
        }

        sys.refresh_memory();
        let available = sys.available_memory();
        let pressured = available < min_free_bytes;
        pool.note_pressure(pressured);
        if !pressured {
            reported_no_victim = false;
            continue;
        }

        if last_reap.is_some_and(|at| at.elapsed() < REAP_COOLDOWN) {
            continue;
        }

        let candidates: Vec<(u32, u64)> = pool
            .live_pids()
            .into_iter()
            .filter_map(|pid| rss_of_pid(pid).map(|rss| (pid, rss)))
            .collect();

        let Some((pid, rss)) = reap_victim(&candidates, available, min_free_bytes) else {
            if !reported_no_victim {
                reported_no_victim = true;
                debug!(
                    available_mb = available / (1024 * 1024),
                    min_free_mb = min_free_bytes / (1024 * 1024),
                    candidates = candidates.len(),
                    "host memory below safety margin, but no eval subprocess is holding enough \
                     to recover it; not reaping"
                );
            }
            continue;
        };

        warn!(
            pid,
            rss_mb = rss / (1024 * 1024),
            available_mb = available / (1024 * 1024),
            min_free_mb = min_free_bytes / (1024 * 1024),
            "host memory below safety margin; reaping the eval subprocess that can recover it"
        );
        if let Err(err) = kill(Pid::from_raw(pid as i32), Signal::SIGKILL) {
            debug!(pid, %err, "eval subprocess already gone before the reap");
        }
        last_reap = Some(Instant::now());
        reported_no_victim = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;
    const MIB: u64 = 1024 * 1024;

    #[test]
    fn memory_guard_bytes_configured_and_adaptive() {
        assert_eq!(memory_guard_bytes(2048, 64 * GIB), 2048 * 1024 * 1024);
        assert_eq!(memory_guard_bytes(0, 4 * GIB), 4 * GIB / 10);
        assert_eq!(memory_guard_bytes(0, 64 * GIB), GIB);
        assert_eq!(memory_guard_bytes(0, 512 * MIB), 128 * MIB);
    }

    #[test]
    fn the_adaptive_margin_never_needs_a_large_share_of_the_host() {
        for total in [512 * MIB, GIB, 2 * GIB, 4 * GIB, 16 * GIB, 128 * GIB] {
            let margin = memory_guard_bytes(0, total);
            assert!(
                margin * 4 <= total,
                "margin {margin} is over a quarter of a {total}-byte host"
            );
        }
    }

    #[test]
    fn a_victim_too_small_to_clear_the_shortfall_is_left_alone() {
        let available = 980 * MIB;
        let min_free = 1024 * MIB;
        assert_eq!(reap_victim(&[(1442, 27 * MIB)], available, min_free), None);
        assert_eq!(
            reap_victim(&[(1442, 44 * MIB)], available, min_free),
            Some((1442, 44 * MIB))
        );
    }

    #[test]
    fn a_victim_holding_no_resident_memory_is_never_reaped() {
        let victims = [(1, 0), (2, 0), (3, 0)];
        assert_eq!(reap_victim(&victims, 980 * MIB, 1024 * MIB), None);
    }

    #[test]
    fn the_largest_sufficient_victim_wins() {
        let victims = [(1, 100 * MIB), (2, 600 * MIB), (3, 300 * MIB)];
        assert_eq!(
            reap_victim(&victims, 900 * MIB, 1024 * MIB),
            Some((2, 600 * MIB))
        );
    }

    #[test]
    fn no_shortfall_means_no_victim() {
        assert_eq!(reap_victim(&[(1, 8 * GIB)], 2 * GIB, GIB), None);
        assert_eq!(reap_victim(&[], 0, GIB), None);
    }
}
