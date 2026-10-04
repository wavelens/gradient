/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::messages::BuildMetrics;
use harmonia_protocol::daemon_wire::types2::{BuildResult, Microseconds};

const BYTES_PER_MB: u64 = 1_048_576;

/// The daemon is reading these from the build's cgroup. A daemon without cgroups or without the
/// `build-resource-usage` feature is leaving all but the CPU times empty.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct ResourceUsage {
    cpu_usec: Option<u64>,
    memory_peak: Option<u64>,
    io_read_bytes: Option<u64>,
    io_write_bytes: Option<u64>,
    oom_kills: Option<u64>,
}

impl ResourceUsage {
    pub(super) fn of(result: &BuildResult) -> Self {
        Self {
            cpu_usec: cpu_usec(result.cpu_user, result.cpu_system),
            memory_peak: result.memory_peak,
            io_read_bytes: result.io_read_bytes,
            io_write_bytes: result.io_write_bytes,
            oom_kills: result.oom_kills,
        }
    }
}

fn cpu_usec(user: Option<Microseconds>, system: Option<Microseconds>) -> Option<u64> {
    let us = |m: Microseconds| m.0.max(0) as u64;
    match (user, system) {
        (None, None) => None,
        (u, s) => Some(u.map(us).unwrap_or(0) + s.map(us).unwrap_or(0)),
    }
}

pub(super) fn build_metrics(usage: ResourceUsage, build_time_ms: u64) -> BuildMetrics {
    observe_disk_speed(usage, build_time_ms);
    metrics_from(
        usage,
        build_time_ms,
        crate::metrics::host_static().cpu_count,
    )
}

fn observe_disk_speed(usage: ResourceUsage, build_time_ms: u64) {
    let bytes = usage.io_read_bytes.unwrap_or(0) + usage.io_write_bytes.unwrap_or(0);
    if build_time_ms > 0 && bytes > 0 {
        let mb_per_s = (bytes as f64 / BYTES_PER_MB as f64) / (build_time_ms as f64 / 1000.0);
        gradient_worker_client::throughput::DISK.observe(mb_per_s);
    }
}

fn metrics_from(usage: ResourceUsage, build_time_ms: u64, cpu_count: u32) -> BuildMetrics {
    let cpu_time_ms = usage.cpu_usec.map(|u| u / 1000);
    let avg_cpu_pct = match cpu_time_ms {
        Some(cpu_ms) if build_time_ms > 0 && cpu_count > 0 => {
            Some(cpu_ms as f32 / (build_time_ms as f32 * cpu_count as f32) * 100.0)
        }
        _ => None,
    };

    BuildMetrics {
        peak_ram_mb: usage.memory_peak.map(|b| b / BYTES_PER_MB),
        cpu_time_ms,
        avg_cpu_pct,
        disk_read_bytes: usage.io_read_bytes,
        disk_write_bytes: usage.io_write_bytes,
        oom_killed: usage.oom_kills.is_some_and(|kills| kills > 0),
        build_time_ms: Some(build_time_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_usec_sums_present_fields() {
        assert_eq!(cpu_usec(None, None), None);
        assert_eq!(cpu_usec(Some(Microseconds(700)), None), Some(700));
        assert_eq!(
            cpu_usec(Some(Microseconds(700)), Some(Microseconds(300))),
            Some(1000),
        );
        assert_eq!(
            cpu_usec(Some(Microseconds(-1)), Some(Microseconds(5))),
            Some(5)
        );
    }

    #[test]
    fn a_daemon_without_resource_usage_leaves_only_the_build_time() {
        let m = metrics_from(ResourceUsage::default(), 5_000, 4);
        assert_eq!(
            m,
            BuildMetrics {
                build_time_ms: Some(5_000),
                ..Default::default()
            }
        );
    }

    #[test]
    fn the_daemon_usage_becomes_the_build_metrics() {
        let usage = ResourceUsage {
            cpu_usec: Some(8_000_000),
            memory_peak: Some(3 * BYTES_PER_MB + 1),
            io_read_bytes: Some(10),
            io_write_bytes: Some(20),
            oom_kills: Some(1),
        };
        assert_eq!(
            metrics_from(usage, 4_000, 4),
            BuildMetrics {
                peak_ram_mb: Some(3),
                cpu_time_ms: Some(8_000),
                avg_cpu_pct: Some(50.0),
                disk_read_bytes: Some(10),
                disk_write_bytes: Some(20),
                oom_killed: true,
                build_time_ms: Some(4_000),
            }
        );
    }

    #[test]
    fn no_out_of_memory_kill_is_not_an_out_of_memory_build() {
        let usage = ResourceUsage {
            oom_kills: Some(0),
            ..Default::default()
        };
        assert!(!metrics_from(usage, 1_000, 4).oom_killed);
    }

    #[test]
    fn the_cpu_share_needs_a_build_time_and_cores() {
        let usage = ResourceUsage {
            cpu_usec: Some(1_000_000),
            ..Default::default()
        };
        assert_eq!(metrics_from(usage, 0, 4).avg_cpu_pct, None);
        assert_eq!(metrics_from(usage, 1_000, 0).avg_cpu_pct, None);
    }
}
