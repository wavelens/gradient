/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Build metrics sampling - peak network throughput and live build cgroup
//! (`memory.peak` / `io.stat`) collection, folded into the wire [`BuildMetrics`].

use gradient_proto::messages::BuildMetrics;
use harmonia_protocol::daemon_wire::types2::Microseconds;
use std::path::{Path, PathBuf};
use tracing::debug;

use crate::metrics::cgroup::{BuildMetricsRaw, read_build_cgroup};

const BYTES_PER_MB: u64 = 1_048_576;
/// Cadence for sampling a live build cgroup's `memory.peak` / `io.stat`.
const CGROUP_SAMPLE_MS: u64 = 200;

/// Convert a raw cgroup snapshot into the wire `BuildMetrics`.
///
/// `build_time_ms` is always reported. Cgroup-derived fields are `None` when
/// `raw` is absent. `avg_cpu_pct` is `None` when it cannot be computed
/// (zero build time or zero CPU count) to avoid divide-by-zero.
fn raw_to_build_metrics(
    raw: Option<BuildMetricsRaw>,
    build_time_ms: u64,
    cpu_count: u32,
    peak_network_mbps: Option<f32>,
) -> BuildMetrics {
    let Some(raw) = raw else {
        return BuildMetrics {
            build_time_ms: Some(build_time_ms),
            peak_network_mbps,
            ..Default::default()
        };
    };

    let cpu_time_ms = raw.cpu_usage_usec.map(|u| u / 1000);
    let avg_cpu_pct = match cpu_time_ms {
        Some(cpu_ms) if build_time_ms > 0 && cpu_count > 0 => {
            Some(cpu_ms as f32 / (build_time_ms as f32 * cpu_count as f32) * 100.0)
        }
        _ => None,
    };

    BuildMetrics {
        peak_ram_mb: raw.peak_ram_bytes.map(|b| b / BYTES_PER_MB),
        cpu_time_ms,
        avg_cpu_pct,
        disk_read_bytes: Some(raw.disk_read_bytes),
        disk_write_bytes: Some(raw.disk_write_bytes),
        oom_killed: raw.oom_killed,
        build_time_ms: Some(build_time_ms),
        peak_network_mbps,
    }
}

/// Locate a running build's cgroup via nix's `<state-dir>/cgroups/` map.
///
/// Nix can't be asked which build user / cgroup it assigned, but on each build
/// it writes the build's absolute cgroup path to `<state-dir>/cgroups/<uid>`.
/// Those files persist after the build (pointing at a since-destroyed cgroup),
/// so we only consider entries modified at/after `since` (the build's start)
/// and return the newest. Returns `None` when nothing is newer than `since`
/// (idle, daemon not using cgroups, or concurrent starts we won't guess at).
fn newest_build_cgroup(state_dir: &Path, since: std::time::SystemTime) -> Option<PathBuf> {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(state_dir).ok()?.flatten() {
        let Ok(mtime) = entry.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        if mtime < since {
            continue;
        }
        if newest.as_ref().is_none_or(|(t, _)| mtime > *t)
            && let Ok(contents) = std::fs::read_to_string(entry.path())
        {
            let path = PathBuf::from(contents.trim());
            if !path.as_os_str().is_empty() {
                newest = Some((mtime, path));
            }
        }
    }
    newest.map(|(_, p)| p)
}

/// Total CPU microseconds from the daemon's `BuildResult` (cgroup-derived,
/// captured by nix before it tears the cgroup down). `None` only when the
/// daemon reported neither user nor system time.
pub(super) fn daemon_cpu_usec(
    user: Option<Microseconds>,
    system: Option<Microseconds>,
) -> Option<u64> {
    let us = |m: Microseconds| m.0.max(0) as u64;
    match (user, system) {
        (None, None) => None,
        (u, s) => Some(u.map(us).unwrap_or(0) + s.map(us).unwrap_or(0)),
    }
}

/// Assemble per-build metrics from the live-sampled cgroup snapshot (peak RAM /
/// disk, captured before teardown) and the daemon-reported CPU time. Always
/// reports `build_time_ms`; cgroup fields stay `None` when no cgroup was
/// sampled (metrics disabled, idle map, or ambiguous concurrent starts).
pub(super) fn assemble_build_metrics(
    sampled: Option<BuildMetricsRaw>,
    cpu_usec: Option<u64>,
    build_time_ms: u64,
    peak_network_mbps: Option<f32>,
) -> BuildMetrics {
    let cpu_count = crate::metrics::host_static().cpu_count;
    let raw = match (sampled, cpu_usec) {
        (None, None) => None,
        (s, cpu) => Some(BuildMetricsRaw {
            cpu_usage_usec: cpu,
            ..s.unwrap_or_default()
        }),
    };
    if let Some(r) = raw.as_ref() {
        let bytes = r.disk_read_bytes + r.disk_write_bytes;
        if build_time_ms > 0 && bytes > 0 {
            let mb_per_s = (bytes as f64 / 1_048_576.0) / (build_time_ms as f64 / 1000.0);
            crate::metrics::throughput::DISK.observe(mb_per_s);
        }
    }
    raw_to_build_metrics(raw, build_time_ms, cpu_count, peak_network_mbps)
}

/// Tracks the host's peak NAR network throughput (Mbps) over a build window.
/// Host-level: cgroup v2 carries no per-build network accounting, so this is
/// the closest honest signal and is exact only when the build is the sole
/// network consumer.
pub(super) struct NetworkPeakSampler {
    peak: std::sync::Arc<std::sync::atomic::AtomicU64>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: tokio::task::JoinHandle<()>,
}

impl NetworkPeakSampler {
    pub(super) fn start() -> Self {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        let peak = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let (p, s) = (peak.clone(), stop.clone());
        let handle = tokio::spawn(async move {
            while !s.load(Ordering::Relaxed) {
                if let Some(v) = crate::metrics::throughput::NETWORK.current() {
                    let bits = (v as f64).to_bits();
                    let mut prev = p.load(Ordering::Relaxed);
                    while f64::from_bits(prev) < v as f64 {
                        match p.compare_exchange_weak(
                            prev,
                            bits,
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => break,
                            Err(cur) => prev = cur,
                        }
                    }
                }
                tokio::time::sleep(std::time::Duration::from_millis(250)).await;
            }
        });
        Self { peak, stop, handle }
    }

    pub(super) async fn finish(self) -> Option<f32> {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.handle.await;
        match self.peak.load(Ordering::Relaxed) {
            0 => None,
            b => Some(f64::from_bits(b) as f32),
        }
    }
}

/// Samples a daemon build's cgroup `memory.peak` / `io.stat` *while it runs*,
/// because nix destroys the cgroup as soon as the build finishes. It locates
/// the cgroup via [`newest_build_cgroup`] (entries written after `start` was
/// called), locks onto it, and keeps the last good reading - the high-water
/// `memory.peak` and cumulative `io.stat` just before teardown.
pub(super) struct CgroupSampler {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    handle: tokio::task::JoinHandle<Option<BuildMetricsRaw>>,
}

impl CgroupSampler {
    pub(super) fn start(state_dir: String, cgroup_root: String) -> Self {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};
        let stop = Arc::new(AtomicBool::new(false));
        let since = std::time::SystemTime::now();
        let s = stop.clone();
        let handle = tokio::spawn(async move {
            let mut cgroup: Option<PathBuf> = None;
            let mut last: Option<BuildMetricsRaw> = None;
            let mut logged = false;
            while !s.load(Ordering::Relaxed) {
                let state_dir = state_dir.clone();
                let cgroup_root = cgroup_root.clone();
                let known = cgroup.clone();
                // The cgroup lookup + read are blocking fs syscalls; run them
                // off the async runtime thread so a slow/contended /sys read
                // never stalls other tasks sharing this worker thread.
                let sample = tokio::task::spawn_blocking(
                    move || -> Option<(PathBuf, Option<BuildMetricsRaw>)> {
                        let dir = Path::new(&state_dir);
                        // Trust only paths under the configured cgroup root.
                        let cgroup = known.or_else(|| {
                            newest_build_cgroup(dir, since).filter(|p| p.starts_with(&cgroup_root))
                        })?;
                        let raw = read_build_cgroup(&cgroup);
                        Some((cgroup, raw))
                    },
                )
                .await
                .ok()
                .flatten();

                match sample {
                    Some((dir, Some(cur))) => {
                        if !logged {
                            debug!(cgroup = %dir.display(), "sampling build cgroup for metrics");
                            logged = true;
                        }
                        cgroup = Some(dir);
                        last = Some(merge_cgroup_sample(last, cur));
                    }
                    Some((_, None)) => break, // cgroup torn down; keep the last good sample
                    None => {}
                }
                tokio::time::sleep(std::time::Duration::from_millis(CGROUP_SAMPLE_MS)).await;
            }
            last
        });
        Self { stop, handle }
    }

    pub(super) async fn finish(self) -> Option<BuildMetricsRaw> {
        use std::sync::atomic::Ordering;
        self.stop.store(true, Ordering::Relaxed);
        self.handle.await.ok().flatten()
    }
}

/// Fold a fresh cgroup reading into the running snapshot: `memory.peak` is a
/// kernel high-water mark so take the max; `io.stat` is cumulative so the latest
/// read is the total; OOM is sticky.
fn merge_cgroup_sample(prev: Option<BuildMetricsRaw>, cur: BuildMetricsRaw) -> BuildMetricsRaw {
    let prev = prev.unwrap_or_default();
    BuildMetricsRaw {
        peak_ram_bytes: prev.peak_ram_bytes.max(cur.peak_ram_bytes),
        cpu_usage_usec: None,
        disk_read_bytes: cur.disk_read_bytes,
        disk_write_bytes: cur.disk_write_bytes,
        oom_killed: prev.oom_killed || cur.oom_killed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_build_cgroup_ignores_entries_older_than_since() {
        // Stale `<uid>` files (from finished builds) must not be picked: their
        // cgroups are already gone. Only an entry written at/after the build
        // started counts. Explicit mtimes keep this independent of filesystem
        // timestamp granularity.
        use std::fs::FileTimes;
        use std::time::Duration;
        let set_mtime = |path: &Path, t: std::time::SystemTime| {
            std::fs::File::options()
                .write(true)
                .open(path)
                .unwrap()
                .set_times(FileTimes::new().set_modified(t))
                .unwrap();
        };
        let dir = tempfile::tempdir().unwrap();
        let since = std::time::SystemTime::now();

        let stale = dir.path().join("30001");
        std::fs::write(&stale, "/sys/fs/cgroup/nix-build-uid-30001\n").unwrap();
        set_mtime(&stale, since - Duration::from_secs(60));
        assert!(newest_build_cgroup(dir.path(), since).is_none());

        let fresh = dir.path().join("30002");
        std::fs::write(&fresh, "/sys/fs/cgroup/nix-build-uid-30002\n").unwrap();
        set_mtime(&fresh, since + Duration::from_secs(60));
        assert_eq!(
            newest_build_cgroup(dir.path(), since),
            Some(PathBuf::from("/sys/fs/cgroup/nix-build-uid-30002")),
        );
    }

    #[test]
    fn newest_build_cgroup_none_for_missing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("cgroups");
        assert!(newest_build_cgroup(&missing, std::time::SystemTime::UNIX_EPOCH).is_none());
    }

    #[test]
    fn daemon_cpu_usec_sums_present_fields() {
        assert_eq!(daemon_cpu_usec(None, None), None);
        assert_eq!(daemon_cpu_usec(Some(Microseconds(700)), None), Some(700));
        assert_eq!(
            daemon_cpu_usec(Some(Microseconds(700)), Some(Microseconds(300))),
            Some(1000),
        );
        // Negative (unset/garbage) clamps to zero rather than underflowing.
        assert_eq!(
            daemon_cpu_usec(Some(Microseconds(-1)), Some(Microseconds(5))),
            Some(5)
        );
    }

    #[test]
    fn raw_to_metrics_always_sets_build_time() {
        let m = raw_to_build_metrics(None, 5_000, 4, None);
        assert_eq!(m.build_time_ms, Some(5_000));
        assert_eq!(m.peak_ram_mb, None);
        assert_eq!(m.cpu_time_ms, None);
        assert_eq!(m.avg_cpu_pct, None);
        assert!(!m.oom_killed);
    }

    #[test]
    fn raw_to_metrics_handles_zero_divisors() {
        let raw = BuildMetricsRaw {
            peak_ram_bytes: Some(2 * BYTES_PER_MB),
            cpu_usage_usec: Some(1_000_000),
            disk_read_bytes: 10,
            disk_write_bytes: 20,
            oom_killed: false,
        };
        // build_time_ms = 0 → avg_cpu_pct None (no divide-by-zero panic).
        let m = raw_to_build_metrics(Some(raw), 0, 4, None);
        assert_eq!(m.avg_cpu_pct, None);
        // cpu_count = 0 → avg_cpu_pct None.
        let m = raw_to_build_metrics(Some(raw), 1_000, 0, None);
        assert_eq!(m.avg_cpu_pct, None);
        assert_eq!(m.peak_ram_mb, Some(2));
        assert_eq!(m.cpu_time_ms, Some(1_000));
        assert_eq!(m.disk_read_bytes, Some(10));
        assert_eq!(m.disk_write_bytes, Some(20));
    }

    #[test]
    fn raw_to_metrics_computes_avg_cpu_pct() {
        let raw = BuildMetricsRaw {
            peak_ram_bytes: None,
            cpu_usage_usec: Some(8_000_000), // 8000 ms of CPU
            disk_read_bytes: 0,
            disk_write_bytes: 0,
            oom_killed: false,
        };
        // 8000 cpu-ms over 4000 ms wall on 4 cores = 8000/(4000*4)*100 = 50%.
        let m = raw_to_build_metrics(Some(raw), 4_000, 4, Some(125.0));
        assert_eq!(m.cpu_time_ms, Some(8_000));
        assert_eq!(m.avg_cpu_pct, Some(50.0));
        assert_eq!(m.peak_network_mbps, Some(125.0));
    }
}
