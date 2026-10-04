/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Instant;

use sysinfo::{CpuRefreshKind, MemoryRefreshKind, RefreshKind, System};

const BYTES_PER_MB: u64 = 1024 * 1024;

pub struct HostStatic {
    pub cpu_count: u32,
    pub ram_total_mb: u64,
}

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

pub struct HostDynamic {
    pub ram_free_mb: u64,
    pub cpu_usage_pct: f32,
}

/// CPU usage needs two samples spaced by at least [`sysinfo::MINIMUM_CPU_UPDATE_INTERVAL`].
/// This function is blocking for one short sleep to return a meaningful percentage.
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
