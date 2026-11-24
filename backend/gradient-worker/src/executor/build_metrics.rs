/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroU32;
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

impl From<&BuildResult> for ResourceUsage {
    fn from(result: &BuildResult) -> Self {
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
    pub build_cores: Option<NonZeroU32>,
    pub cpu_core_score: u32,
}

pub(super) static RUNNING_BUILDS: RunningBuilds = RunningBuilds::new();

struct RunningBuildPeakUnreclaimableRam {
    job_id: String,
    max_unreclaimable_memory_mb: Option<u64>,
}

pub(super) struct RunningBuilds {
    count: AtomicU32,
    job_id_and_sampled_peak_ram_by_derivation_hash:
        Mutex<BTreeMap<String, RunningBuildPeakUnreclaimableRam>>,
}

impl RunningBuilds {
    const fn new() -> Self {
        Self {
            count: AtomicU32::new(0),
            job_id_and_sampled_peak_ram_by_derivation_hash: Mutex::new(BTreeMap::new()),
        }
    }

    pub(super) fn start(&self, job_id: &str, drv_path: &str) -> RunningBuild<'_> {
        let drv_hash = drv_hash(drv_path).to_owned();
        self.job_id_and_sampled_peak_ram_by_derivation_hash
            .lock()
            .insert(
                drv_hash.clone(),
                RunningBuildPeakUnreclaimableRam {
                    job_id: job_id.to_owned(),
                    max_unreclaimable_memory_mb: None,
                },
            );
        RunningBuild {
            others: self.count.fetch_add(1, Ordering::Relaxed),
            drv_hash,
            builds: self,
        }
    }

    /// The daemon names the cgroup of a build `nix-build@<derivation hash>-<build user>`.
    fn raise_max_unreclaimable_ram_from_nix_build_child_cgroups(&self, daemon_cgroup: &Path) {
        let Ok(cgroups) = std::fs::read_dir(daemon_cgroup) else {
            return;
        };
        let mut unreclaimable_ram_mb_by_derivation_hash: HashMap<String, u64> = HashMap::new();
        for cgroup in cgroups.flatten() {
            let name = cgroup.file_name();
            let Some(hash) = name
                .to_str()
                .and_then(|name| name.strip_prefix("nix-build@"))
                .map(drv_hash)
                .filter(|hash| {
                    self.job_id_and_sampled_peak_ram_by_derivation_hash
                        .lock()
                        .contains_key(*hash)
                })
            else {
                continue;
            };
            if let Ok(memory_stat) = std::fs::read_to_string(cgroup.path().join("memory.stat")) {
                let mb = parse_cgroup_memory_stat_unreclaimable_bytes(&memory_stat) / BYTES_PER_MB;
                let peak = unreclaimable_ram_mb_by_derivation_hash
                    .entry(hash.to_owned())
                    .or_default();
                *peak = (*peak).max(mb);
            }
        }

        let mut builds = self.job_id_and_sampled_peak_ram_by_derivation_hash.lock();
        for (hash, mb) in unreclaimable_ram_mb_by_derivation_hash {
            if let Some(build) = builds.get_mut(&hash) {
                build.max_unreclaimable_memory_mb = Some(
                    build
                        .max_unreclaimable_memory_mb
                        .map_or(mb, |peak| peak.max(mb)),
                );
            }
        }
    }

    fn peak_ram_mb(&self) -> Vec<(String, u64)> {
        self.job_id_and_sampled_peak_ram_by_derivation_hash
            .lock()
            .values()
            .filter_map(|build| Some((build.job_id.clone(), build.max_unreclaimable_memory_mb?)))
            .collect()
    }
}

fn drv_hash(drv_path: &str) -> &str {
    let base = drv_path.strip_prefix("/nix/store/").unwrap_or(drv_path);
    base.split('-').next().unwrap_or(base)
}

fn parse_cgroup_memory_stat_unreclaimable_bytes(memory_stat: &str) -> u64 {
    let memory_stat_bytes_by_key: HashMap<&str, u64> = memory_stat
        .lines()
        .filter_map(|line| {
            let (key, value) = line.split_once(' ')?;
            Some((key, value.trim().parse().ok()?))
        })
        .collect();
    let memory_stat_key_byte_or_zero =
        |key: &str| memory_stat_bytes_by_key.get(key).copied().unwrap_or(0);
    if memory_stat_bytes_by_key.contains_key("kernel") {
        (memory_stat_key_byte_or_zero("anon")
            + memory_stat_key_byte_or_zero("shmem")
            + memory_stat_key_byte_or_zero("kernel")
            + memory_stat_key_byte_or_zero("sock"))
        .saturating_sub(memory_stat_key_byte_or_zero("slab_reclaimable"))
    } else {
        [
            "anon",
            "shmem",
            "slab_unreclaimable",
            "kernel_stack",
            "pagetables",
            "percpu",
            "sock",
        ]
        .into_iter()
        .map(memory_stat_key_byte_or_zero)
        .sum()
    }
}

pub(crate) fn sample_running_build_unreclaimable_ram_maxima_from_nix_daemon_cgroup(
    daemon_cgroup: &Path,
) {
    RUNNING_BUILDS.raise_max_unreclaimable_ram_from_nix_build_child_cgroups(daemon_cgroup);
}

pub(crate) fn worker_has_any_running_build() -> bool {
    RUNNING_BUILDS.count.load(Ordering::Relaxed) > 0
}

pub(crate) fn peak_ram_of_running_builds(
    daemon_cgroup: Option<&Path>,
) -> Option<Vec<(String, u64)>> {
    daemon_cgroup.filter(|cgroup| cgroup.is_dir())?;
    Some(RUNNING_BUILDS.peak_ram_mb())
}

pub(super) struct RunningBuild<'a> {
    others: u32,
    drv_hash: String,
    builds: &'a RunningBuilds,
}

impl RunningBuild<'_> {
    fn largest_sampled_unreclaimable_ram_mb(&self) -> Option<u64> {
        self.builds
            .job_id_and_sampled_peak_ram_by_derivation_hash
            .lock()
            .get(&self.drv_hash)
            .and_then(|build| build.max_unreclaimable_memory_mb)
    }
}

impl Drop for RunningBuild<'_> {
    fn drop(&mut self) {
        self.builds.count.fetch_sub(1, Ordering::Relaxed);
        self.builds
            .job_id_and_sampled_peak_ram_by_derivation_hash
            .lock()
            .remove(&self.drv_hash);
    }
}

pub(super) fn build_metrics(
    usage: ResourceUsage,
    build_time_ms: u64,
    host: BuildHost,
    cores: u32,
    running: &RunningBuild<'_>,
) -> BuildMetrics {
    observe_disk_speed(usage, build_time_ms);
    let cpu_count = crate::metrics::host_static().cpu_count;
    BuildMetrics {
        concurrent_builds: Some(running.others),
        build_cores: Some(cores),
        cpu_core_score: Some(host.cpu_core_score),
        ..metrics_from(
            usage,
            build_time_ms,
            cpu_count,
            running.largest_sampled_unreclaimable_ram_mb(),
        )
    }
}

pub(super) fn build_cores_for(spec: Option<u32>, host: Option<NonZeroU32>, cpu_count: u32) -> u32 {
    let host = host.map_or(cpu_count, |cores| cores.get().min(cpu_count));
    spec.map_or(host, |cores| cores.min(host)).max(1)
}

fn observe_disk_speed(usage: ResourceUsage, build_time_ms: u64) {
    let bytes = usage.io_read_bytes.unwrap_or(0) + usage.io_write_bytes.unwrap_or(0);
    if build_time_ms > 0 && bytes > 0 {
        let mb_per_s = (bytes as f64 / BYTES_PER_MB as f64) / (build_time_ms as f64 / 1000.0);
        gradient_worker_client::throughput::DISK.observe(mb_per_s);
    }
}

fn metrics_from(
    usage: ResourceUsage,
    build_time_ms: u64,
    cpu_count: u32,
    max_sampled_unreclaimable_ram_mb: Option<u64>,
) -> BuildMetrics {
    let cpu_time_ms = usage.cpu_usec.map(|u| u / 1000);
    let avg_cpu_pct = match cpu_time_ms {
        Some(cpu_ms) if build_time_ms > 0 && cpu_count > 0 => {
            Some(cpu_ms as f32 / (build_time_ms as f32 * cpu_count as f32) * 100.0)
        }
        _ => None,
    };

    BuildMetrics {
        peak_ram_mb: max_sampled_unreclaimable_ram_mb
            .or(usage.memory_peak.map(|b| b / BYTES_PER_MB)),
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
        let m = metrics_from(ResourceUsage::default(), 5_000, 4, None);
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
            metrics_from(usage, 4_000, 4, None),
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
        assert!(!metrics_from(usage, 1_000, 4, None).oom_killed);
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
    fn test_parse_cgroup_memory_stat_unreclaimable_bytes_counts_kernel_minus_slab_reclaimable_and_ignores_file()
     {
        let stat = "anon 1000\nfile 900000\nkernel 300\nkernel_stack 40\nslab 120\n\
                    slab_reclaimable 100\nslab_unreclaimable 20\nshmem 50\nsock 3\nfile_dirty 7\n";
        assert_eq!(parse_cgroup_memory_stat_unreclaimable_bytes(stat), 1253);
    }

    #[test]
    fn parse_cgroup_memory_stat_unreclaimable_bytes_without_kernel_key() {
        let stat = "anon 1000\nfile 900000\nkernel_stack 40\npagetables 30\npercpu 5\n\
                    sock 3\nshmem 50\nslab_reclaimable 100\nslab_unreclaimable 20\n";
        assert_eq!(parse_cgroup_memory_stat_unreclaimable_bytes(stat), 1148);
        assert_eq!(
            parse_cgroup_memory_stat_unreclaimable_bytes("anon 7\nfile 9\n"),
            7
        );
        assert_eq!(parse_cgroup_memory_stat_unreclaimable_bytes(""), 0);
    }

    #[test]
    fn running_build_peak_unreclaimable_memory_keeps_highest_sample_after_memory_stat_drop_and_ignores_unknown_derivation_hash_cgroups()
     {
        let daemon = tempfile::TempDir::new().unwrap();
        let cgroup = |name: &str, anon_bytes: u64| {
            let dir = daemon.path().join(name);
            std::fs::create_dir_all(&dir).unwrap();
            let stat = format!("anon {anon_bytes}\nfile {}\n", 50 * BYTES_PER_MB);
            std::fs::write(dir.join("memory.stat"), stat).unwrap();
        };
        cgroup("nix-build@aaaa-nixbld1", 7 * BYTES_PER_MB);
        cgroup("nix-build@bbbb-nixbld2", 9 * BYTES_PER_MB);
        cgroup("nix-daemon", 11 * BYTES_PER_MB);

        let builds = RunningBuilds::new();
        let running = builds.start("job-1", "/nix/store/aaaa-hello-2.12.drv");
        assert_eq!(builds.peak_ram_mb(), vec![]);

        builds.raise_max_unreclaimable_ram_from_nix_build_child_cgroups(daemon.path());
        cgroup("nix-build@aaaa-nixbld1", 3 * BYTES_PER_MB);
        builds.raise_max_unreclaimable_ram_from_nix_build_child_cgroups(daemon.path());
        assert_eq!(builds.peak_ram_mb(), vec![("job-1".to_owned(), 7)]);
        assert_eq!(running.largest_sampled_unreclaimable_ram_mb(), Some(7));

        drop(running);
        assert_eq!(builds.peak_ram_mb(), vec![]);
    }

    #[test]
    fn final_build_metrics_use_unreclaimable_peak_over_daemon_peak_and_keep_daemon_value_without_samples()
     {
        let usage = ResourceUsage {
            memory_peak: Some(40 * BYTES_PER_MB),
            ..Default::default()
        };
        assert_eq!(metrics_from(usage, 1_000, 4, Some(6)).peak_ram_mb, Some(6));
        assert_eq!(metrics_from(usage, 1_000, 4, None).peak_ram_mb, Some(40));
    }

    #[test]
    fn the_cores_of_the_build_spec_win_over_the_host_but_never_exceed_it() {
        assert_eq!(build_cores_for(Some(20), NonZeroU32::new(32), 64), 20);
        assert_eq!(build_cores_for(Some(48), NonZeroU32::new(32), 64), 32);
        assert_eq!(build_cores_for(None, NonZeroU32::new(64), 16), 16);
        assert_eq!(build_cores_for(None, None, 64), 64);
        assert_eq!(build_cores_for(Some(0), None, 8), 1);
    }

    #[test]
    fn the_cpu_share_needs_a_build_time_and_cores() {
        let usage = ResourceUsage {
            cpu_usec: Some(1_000_000),
            ..Default::default()
        };
        assert_eq!(metrics_from(usage, 0, 4, None).avg_cpu_pct, None);
        assert_eq!(metrics_from(usage, 1_000, 0, None).avg_cpu_pct, None);
    }
}
