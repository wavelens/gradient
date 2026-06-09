/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Host capability and live-metrics sampling for the scheduler scoring model.
//!
//! Static caps ([`host_static`], [`cpu_core_score`]) are sampled once at setup
//! and advertised via `WorkerCapabilities`. Dynamic load ([`host_dynamic`]) is
//! sampled each heartbeat and sent via `WorkerMetrics`.

pub mod cgroup;
pub mod throughput;

use std::time::Instant;

use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

const BYTES_PER_MB: u64 = 1024 * 1024;

/// Fixed-capability values that don't change over the worker's lifetime.
pub struct HostStatic {
    pub cpu_count: u32,
    pub ram_total_mb: u64,
}

/// Sample the host's static capabilities (logical CPU count, total RAM).
pub fn host_static() -> HostStatic {
    let sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    HostStatic {
        cpu_count: sys.cpus().len().max(1) as u32,
        ram_total_mb: (sys.total_memory() / BYTES_PER_MB).max(1),
    }
}

/// Live host load, sampled fresh on each call.
pub struct HostDynamic {
    pub ram_free_mb: u64,
    pub cpu_usage_pct: f32,
}

/// Sample live host load. CPU usage requires two samples spaced by at least
/// [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`]; this is encapsulated here with a
/// single short blocking sleep so callers get a meaningful percentage.
pub fn host_dynamic() -> HostDynamic {
    let mut sys = System::new_with_specifics(
        RefreshKind::nothing()
            .with_cpu(CpuRefreshKind::nothing().with_cpu_usage())
            .with_memory(MemoryRefreshKind::nothing().with_ram()),
    );
    sys.refresh_cpu_all();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    sys.refresh_cpu_all();
    sys.refresh_memory();
    HostDynamic {
        ram_free_mb: sys.available_memory() / BYTES_PER_MB,
        cpu_usage_pct: sys.global_cpu_usage(),
    }
}

const BENCH_ITERATIONS: u64 = 5_000_000;
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;
const SCORE_MIN: u32 = 1;
const SCORE_MAX: u32 = 100_000;

/// Deterministic single-core micro-benchmark. Runs a fixed-iteration FNV-style
/// integer hash loop and converts elapsed time into ops-per-ms, scaled and
/// clamped to `1..=100_000`. Higher = faster core. The result is purely a
/// relative speed indicator for cross-worker comparison.
pub fn cpu_core_score() -> u32 {
    let start = Instant::now();
    let mut hash: u64 = FNV_OFFSET;
    for i in 0..BENCH_ITERATIONS {
        hash ^= i;
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    let elapsed = start.elapsed();
    std::hint::black_box(hash);

    let elapsed_ms = elapsed.as_secs_f64() * 1_000.0;
    if elapsed_ms <= 0.0 {
        return SCORE_MAX;
    }
    let ops_per_ms = BENCH_ITERATIONS as f64 / elapsed_ms;
    let score = (ops_per_ms / 100.0).round() as i64;
    score.clamp(SCORE_MIN as i64, SCORE_MAX as i64) as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_core_score_in_bounds_and_positive() {
        let s = cpu_core_score();
        assert!((1..=100_000).contains(&s));
    }

    #[test]
    fn host_static_reports_nonzero() {
        let h = host_static();
        assert!(h.cpu_count >= 1);
        assert!(h.ram_total_mb >= 1);
    }
}
