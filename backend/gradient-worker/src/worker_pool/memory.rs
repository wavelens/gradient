/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Memory accounting for the eval pool: the pool-size budget, the free-RAM
//! guard margin, and the background reaper that converts host memory pressure
//! into one bounded eval failure instead of a host OOM.

use std::sync::Weak;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

use super::pool::EvalWorkerPool;

/// How often the reaper samples host `MemAvailable`.
const REAPER_INTERVAL: Duration = Duration::from_millis(500);

/// Quiet window after a kill. Reclaiming the victim's pages is not instant (the
/// parent has to reap it and the kernel has to hand the pages back), so without
/// this the next tick still reads the pre-kill `MemAvailable` and reaps again.
const REAP_COOLDOWN: Duration = Duration::from_secs(5);

/// Eval-pool size that keeps `size * max_eval_rss` within `ram_budget` (the
/// no-OOM invariant), capped at the configured `fork_workers` and floored at 1
/// so even a tiny host still evaluates - one shard at a time, slower, but it
/// completes. Lowering `max_eval_rss` therefore trades parallelism for a smaller
/// footprint, never the ability to finish.
pub fn budgeted_pool_size(fork_workers: usize, max_eval_rss: u64, ram_budget: u64) -> usize {
    let mem_bound = (ram_budget / max_eval_rss.max(1)).max(1) as usize;

    fork_workers.min(mem_bound).max(1)
}

/// Adaptive free-RAM margin (bytes): the configured `min_free_ram_mb` if set,
/// else 10% of total RAM clamped to `[128 MiB, 1 GiB]`. Below this the reaper
/// acts and `acquire` back-pressures. Lifted out for unit testing.
///
/// The 1 GiB is a **ceiling**, not a floor. As a floor it demanded half of a
/// 2 GiB host be free at all times, so the guard was armed continuously under
/// ordinary build load and the reaper spent the whole run killing evals that
/// were never the cause (#579). The margin only has to be deep enough to react
/// before the kernel OOM-kills, which is an absolute quantity - scaling it past
/// a gigabyte buys nothing, and letting it outgrow the host breaks it.
pub fn memory_guard_bytes(min_free_ram_mb: u64, total_ram_bytes: u64) -> u64 {
    const MIN: u64 = 128 * 1024 * 1024;
    const MAX: u64 = 1024 * 1024 * 1024;
    if min_free_ram_mb > 0 {
        min_free_ram_mb * 1024 * 1024
    } else {
        (total_ram_bytes / 10).clamp(MIN, MAX)
    }
}

/// The eval subprocess worth reaping out of `candidates` (`(pid, rss)`), if any.
///
/// Only a victim whose RSS covers the entire shortfall is worth killing. A
/// smaller one leaves the host still under the margin, so the next tick reaps
/// again and the pool is walked to death one eval at a time - the observed
/// failure was a 27 MiB victim taken to recover a 44 MiB shortfall, alongside
/// hundreds of kills of processes holding no resident memory at all (#579). If
/// nothing is big enough, the pressure is not coming from evaluation and no kill
/// can fix it; `acquire` back-pressure still keeps the pool from growing.
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

/// RSS (bytes) of an arbitrary pid from `/proc/<pid>/statm` (field 2 = resident
/// pages x 4 KiB). `None` if the pid is gone or the read fails. A sub-page
/// procfs read; cheap enough for async callers without a spawn_blocking hop.
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

/// Background memory guard: when host `MemAvailable` drops below
/// `min_free_bytes`, SIGKILL the one live eval subprocess large enough to bring
/// it back above the margin, so a runaway evaluation cannot take the whole host
/// down. The victim's parent task then sees its pipe close and reports the eval
/// failed - converting a would-be host OOM (which could kill the worker itself
/// and strand the job, since the server only learns of a clean disconnect) into
/// a single bounded eval failure.
///
/// A kill is only ever worth its cost when it actually clears the pressure: an
/// eval killed mid-run loses its work and, if it was mid-checkpoint, leaves an
/// uncommitted eval cache behind. See [`reap_victim`] for when that holds.
///
/// Exits when the pool is dropped (worker shutdown). A no-op when disabled.
pub(super) async fn memory_reaper_loop(pool: Weak<EvalWorkerPool>, min_free_bytes: u64) {
    if min_free_bytes == 0 {
        return;
    }

    use sysinfo::{MemoryRefreshKind, RefreshKind, System};
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing().with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    let mut interval = tokio::time::interval(REAPER_INTERVAL);
    let mut last_reap: Option<Instant> = None;
    // One line per pressure episode, not one per 500 ms tick.
    let mut reported_no_victim = false;
    loop {
        interval.tick().await;
        let Some(pool) = pool.upgrade() else {
            return;
        };

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
        #[cfg(unix)]
        unsafe {
            libc::kill(pid as i32, libc::SIGKILL);
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
    fn budgeted_pool_size_caps_by_memory() {
        // 8 GiB box, 2 GiB cap, 75% budget (6 GiB) -> 3 shards, capped at cores.
        assert_eq!(budgeted_pool_size(16, 2 * GIB, 6 * GIB), 3);
        // Plenty of RAM -> the configured worker count wins.
        assert_eq!(budgeted_pool_size(8, 2 * GIB, 256 * GIB), 8);
        // Cap >= budget -> still one worker (slower, but never zero).
        assert_eq!(budgeted_pool_size(16, 8 * GIB, 6 * GIB), 1);
        // Degenerate cap never divides by zero.
        assert_eq!(budgeted_pool_size(4, 0, 6 * GIB), 4);
    }

    #[test]
    fn memory_guard_bytes_configured_and_adaptive() {
        // A configured margin wins, converted MiB -> bytes.
        assert_eq!(memory_guard_bytes(2048, 64 * GIB), 2048 * 1024 * 1024);
        // Adaptive: 10% of total, in band.
        assert_eq!(memory_guard_bytes(0, 4 * GIB), 4 * GIB / 10);
        // Capped at 1 GiB - a bigger host does not need a deeper margin to
        // notice pressure before the kernel does.
        assert_eq!(memory_guard_bytes(0, 64 * GIB), GIB);
        // Floored at 128 MiB so a tiny host still has a reaction window.
        assert_eq!(memory_guard_bytes(0, 512 * MIB), 128 * MIB);
    }

    /// The margin must stay a small fraction of the host. As a 1 GiB *floor* it
    /// demanded half of the 2 GiB CI builder be free, so the guard was armed
    /// continuously under ordinary build load (#579).
    #[test]
    fn the_adaptive_margin_never_demands_a_large_share_of_the_host() {
        for total in [512 * MIB, GIB, 2 * GIB, 4 * GIB, 16 * GIB, 128 * GIB] {
            let margin = memory_guard_bytes(0, total);
            assert!(
                margin * 4 <= total,
                "margin {margin} is over a quarter of a {total}-byte host"
            );
        }
    }

    /// The kill that broke the `gradient-cache` run: 27 MiB reaped against a
    /// 44 MiB shortfall, which cannot lift the host back over the margin and so
    /// only guarantees the next tick reaps again.
    #[test]
    fn a_victim_too_small_to_clear_the_shortfall_is_left_alone() {
        let available = 980 * MIB;
        let min_free = 1024 * MIB;
        assert_eq!(reap_victim(&[(1442, 27 * MIB)], available, min_free), None);
        // 44 MiB exactly covers the shortfall, so it is worth the kill.
        assert_eq!(
            reap_victim(&[(1442, 44 * MIB)], available, min_free),
            Some((1442, 44 * MIB))
        );
    }

    /// ~336 of the run's 338 kills had `rss_mb=0`: freeing nothing while still
    /// destroying an eval and its uncommitted eval cache.
    #[test]
    fn a_victim_holding_no_resident_memory_is_never_reaped() {
        let victims = [(1, 0), (2, 0), (3, 0)];
        assert_eq!(reap_victim(&victims, 980 * MIB, 1024 * MIB), None);
    }

    /// Among victims that do clear the shortfall, the largest still wins - a
    /// runaway eval is the one the guard exists for.
    #[test]
    fn the_largest_sufficient_victim_wins() {
        let victims = [(1, 100 * MIB), (2, 600 * MIB), (3, 300 * MIB)];
        assert_eq!(
            reap_victim(&victims, 900 * MIB, 1024 * MIB),
            Some((2, 600 * MIB))
        );
    }

    /// No pressure, no kill - even with a huge eval resident.
    #[test]
    fn no_shortfall_means_no_victim() {
        assert_eq!(reap_victim(&[(1, 8 * GIB)], 2 * GIB, GIB), None);
        assert_eq!(reap_victim(&[], 0, GIB), None);
    }
}
