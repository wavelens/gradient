/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Memory accounting for the eval pool: the pool-size budget, the free-RAM
//! guard margin, and the background reaper that converts host memory pressure
//! into one bounded eval failure instead of a host OOM.

use std::sync::Weak;
use std::time::Duration;
use tracing::warn;

use super::pool::EvalWorkerPool;

/// How often the reaper samples host `MemAvailable`.
const REAPER_INTERVAL: Duration = Duration::from_millis(500);

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
/// else `max(1 GiB, 10% of total RAM)`. Below this the reaper acts and `acquire`
/// back-pressures. Lifted out for unit testing.
pub fn memory_guard_bytes(min_free_ram_mb: u64, total_ram_bytes: u64) -> u64 {
    if min_free_ram_mb > 0 {
        min_free_ram_mb * 1024 * 1024
    } else {
        (total_ram_bytes / 10).max(1024 * 1024 * 1024)
    }
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
/// `min_free_bytes`, SIGKILL the largest live eval subprocess so a runaway
/// evaluation cannot take the whole host down. The victim's parent task then
/// sees its pipe close and reports the eval failed - converting a would-be host
/// OOM (which could kill the worker itself and strand the job, since the server
/// only learns of a clean disconnect) into a single bounded eval failure.
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
            continue;
        }

        let victim = pool
            .live_pids()
            .into_iter()
            .filter_map(|pid| rss_of_pid(pid).map(|rss| (pid, rss)))
            .max_by_key(|&(_, rss)| rss);
        if let Some((pid, rss)) = victim {
            warn!(
                pid,
                rss_mb = rss / (1024 * 1024),
                available_mb = available / (1024 * 1024),
                min_free_mb = min_free_bytes / (1024 * 1024),
                "host memory below safety margin; reaping largest eval subprocess to avoid OOM"
            );
            #[cfg(unix)]
            unsafe {
                libc::kill(pid as i32, libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GIB: u64 = 1024 * 1024 * 1024;

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
        // Adaptive: 10% of total when that clears the 1 GiB floor.
        assert_eq!(memory_guard_bytes(0, 64 * GIB), 64 * GIB / 10);
        // Adaptive floor: at least 1 GiB on a small host (10% of 4 GiB < 1 GiB).
        assert_eq!(memory_guard_bytes(0, 4 * GIB), GIB);
    }
}
