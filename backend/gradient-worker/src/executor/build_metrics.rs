/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use gradient_util::sync::Mutex;
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

#[derive(Debug, Clone, Copy)]
pub struct BuildHost {
    pub build_cores: u32,
    pub cpu_core_score: u32,
}

pub(super) static RUNNING_BUILDS: RunningBuilds = RunningBuilds::new();

pub(super) struct RunningBuilds {
    count: AtomicU32,
    job_of_drv_hash: Mutex<BTreeMap<String, String>>,
}

impl RunningBuilds {
    const fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            job_of_drv_hash: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn start(&self, job_id: &str, drv_path: &str) -> RunningBuild<'_> {
        let drv_hash = drv_hash(drv_path).to_owned();
        self.job_of_drv_hash
            .lock()
            .insert(drv_hash.clone(), job_id.to_owned());
        RunningBuild {
            others: self.count.fetch_add(1, Ordering::Relaxed),
            drv_hash,
            builds: self,
        }
    }

    /// The daemon names the cgroup of a build `nix-build@<derivation hash>-<build user>`.
    fn peak_ram_mb(&self, daemon_cgroup: &Path) -> Option<Vec<(String, u64)>> {
        let job_of_drv_hash = self.job_of_drv_hash.lock().clone();
        let mut peaks: HashMap<String, u64> = HashMap::new();
        for cgroup in std::fs::read_dir(daemon_cgroup).ok()?.flatten() {
            let name = cgroup.file_name();
            let job_id = name
                .to_str()
                .and_then(|name| name.strip_prefix("nix-build@"))
                .and_then(|build| job_of_drv_hash.get(drv_hash(build)));
            if let Some(job_id) = job_id
                && let Some(bytes) = memory_peak(&cgroup.path())
            {
                let peak = peaks.entry(job_id.clone()).or_default();
                *peak = (*peak).max(bytes / BYTES_PER_MB);
            }
        }

        Some(peaks.into_iter().collect())
    }
}

fn drv_hash(drv_path: &str) -> &str {
    let base = drv_path.strip_prefix("/nix/store/").unwrap_or(drv_path);
    base.split('-').next().unwrap_or(base)
}

fn memory_peak(cgroup: &Path) -> Option<u64> {
    std::fs::read_to_string(cgroup.join("memory.peak"))
        .ok()?
        .trim()
        .parse()
        .ok()
}

pub(crate) fn peak_ram_of_running_builds(
    daemon_cgroup: Option<&Path>,
) -> Option<Vec<(String, u64)>> {
    RUNNING_BUILDS.peak_ram_mb(daemon_cgroup?)
}

pub(super) struct RunningBuild<'a> {
    others: u32,
    drv_hash: String,
    builds: &'a RunningBuilds,
}

impl Drop for RunningBuild<'_> {
    fn drop(&mut self) {
        self.builds.count.fetch_sub(1, Ordering::Relaxed);
        self.builds.job_of_drv_hash.lock().remove(&self.drv_hash);
    }
}

pub(super) fn build_metrics(
    usage: ResourceUsage,
    build_time_ms: u64,
    host: BuildHost,
    running: &RunningBuild<'_>,
) -> BuildMetrics {
    observe_disk_speed(usage, build_time_ms);
    let cpu_count = crate::metrics::host_static().cpu_count;
    BuildMetrics {
        concurrent_builds: Some(running.others),
        build_cores: Some(effective_cores(host.build_cores, cpu_count)),
        cpu_core_score: Some(host.cpu_core_score),
        ..metrics_from(usage, build_time_ms, cpu_count)
    }
}

fn effective_cores(build_cores: u32, cpu_count: u32) -> u32 {
    if build_cores == 0 {
        cpu_count
    } else {
        build_cores.min(cpu_count)
    }
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
        ..Default::default()
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
                ..Default::default()
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
    fn a_build_counts_the_builds_running_beside_it() {
        let builds = RunningBuilds::new();
        let first = builds.start("job-1", "aaaa-first.drv");
        let second = builds.start("job-2", "bbbb-second.drv");
        drop(first);
        let third = builds.start("job-3", "cccc-third.drv");

        assert_eq!((second.others, third.others), (1, 1));
    }

    #[test]
    fn a_running_build_reports_the_peak_memory_of_its_cgroup() {
        let daemon = tempfile::TempDir::new().unwrap();
        let cgroup = |name: &str, peak_bytes: u64| {
            let dir = daemon.path().join(name);
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("memory.peak"), format!("{peak_bytes}\n")).unwrap();
        };
        cgroup("nix-build@aaaa-30001", 3 * BYTES_PER_MB);
        cgroup("nix-build@bbbb-30002", 7 * BYTES_PER_MB);
        cgroup("nix-daemon", 9 * BYTES_PER_MB);

        let builds = RunningBuilds::new();
        let running = builds.start("job-1", "/nix/store/aaaa-hello-2.12.drv");
        assert_eq!(
            builds.peak_ram_mb(daemon.path()),
            Some(vec![("job-1".to_owned(), 3)])
        );

        drop(running);
        assert_eq!(builds.peak_ram_mb(daemon.path()), Some(vec![]));
        assert_eq!(builds.peak_ram_mb(&daemon.path().join("missing")), None);
    }

    #[test]
    fn zero_build_cores_is_every_core_and_a_larger_value_is_capped() {
        assert_eq!(effective_cores(0, 16), 16);
        assert_eq!(effective_cores(4, 16), 4);
        assert_eq!(effective_cores(64, 16), 16);
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
