/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use gradient_wire::guest::AgentToWorker;
use gradient_wire::types::ExecMetrics;
use nix::sys::statvfs::statvfs;
use tokio::sync::{mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

use crate::listener::Progress;
use crate::run::WORK_DIR;

const RESOURCE_USAGE_SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const SAMPLES_PER_RESOURCE_USAGE_WINDOW: u32 = 10;
const FULL_RESOURCE_USAGE_WINDOW_MS: u32 = 10_000;

#[derive(Clone, Copy, Debug, PartialEq)]
struct AggregateCpuTickCountersWithOnlineCpuCount {
    busy: u64,
    total: u64,
    cpus: u32,
}

impl AggregateCpuTickCountersWithOnlineCpuCount {
    fn average_busy_cpu_count_since_earlier_reading(&self, earlier: &Self) -> f32 {
        let total = self.total.saturating_sub(earlier.total);
        if total == 0 {
            return 0.0;
        }
        let busy = self.busy.saturating_sub(earlier.busy);
        busy as f32 / total as f32 * self.cpus as f32
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct OneSecondCpuMemoryDiskUsageSample {
    cpus_in_use: f32,
    ram_used_bytes: u64,
    ram_total_bytes: u64,
    disk_used_bytes: u64,
    disk_total_bytes: u64,
}

struct ExecMetricsWindowAccumulator {
    next_window_number: u32,
    current_window_one_second_samples: Vec<OneSecondCpuMemoryDiskUsageSample>,
}

impl ExecMetricsWindowAccumulator {
    fn new() -> Self {
        Self {
            next_window_number: 0,
            current_window_one_second_samples: Vec::new(),
        }
    }

    fn push(&mut self, sample: OneSecondCpuMemoryDiskUsageSample) {
        self.current_window_one_second_samples.push(sample);
    }

    fn take_window(&mut self, window_ms: u32) -> Option<ExecMetrics> {
        let window_samples = std::mem::take(&mut self.current_window_one_second_samples);
        let latest_window_sample = *window_samples.last()?;
        let sample_count_in_window = window_samples.len();

        let sequence = self.next_window_number;
        self.next_window_number += 1;
        Some(ExecMetrics {
            sequence,
            window_ms,
            cpu_avg: window_samples.iter().map(|s| s.cpus_in_use).sum::<f32>()
                / sample_count_in_window as f32,
            cpu_max: window_samples
                .iter()
                .map(|s| s.cpus_in_use)
                .fold(0.0, f32::max),
            ram_avg_bytes: window_samples.iter().map(|s| s.ram_used_bytes).sum::<u64>()
                / sample_count_in_window as u64,
            ram_max_bytes: window_samples
                .iter()
                .map(|s| s.ram_used_bytes)
                .max()
                .unwrap_or_default(),
            ram_total_bytes: latest_window_sample.ram_total_bytes,
            disk_max_bytes: window_samples
                .iter()
                .map(|s| s.disk_used_bytes)
                .max()
                .unwrap_or_default(),
            disk_total_bytes: latest_window_sample.disk_total_bytes,
        })
    }
}

pub async fn sample_and_send_exec_metrics_windows_until_shutdown(
    mut progress: watch::Receiver<Progress>,
    outgoing: mpsc::Sender<AgentToWorker>,
) {
    let mut exec_metrics_window_accumulator = ExecMetricsWindowAccumulator::new();
    let mut previous_cpu_tick_counters_reading = read_proc_stat_cpu_tick_counters();
    let mut usage_window_start_instant = Instant::now();
    let mut ticks_in_current_window = 0;
    let mut one_second_sample_interval = tokio::time::interval_at(
        usage_window_start_instant + RESOURCE_USAGE_SAMPLE_INTERVAL,
        RESOURCE_USAGE_SAMPLE_INTERVAL,
    );
    one_second_sample_interval.set_missed_tick_behavior(MissedTickBehavior::Delay);

    while progress.borrow().outcome.is_none() {
        tokio::select! {
            _ = one_second_sample_interval.tick() => {
                if let Some(sample) = read_system_usage_sample_from_proc_meminfo_and_work_disk_using_cpu_tick_difference(
                    &mut previous_cpu_tick_counters_reading,
                ) {
                    exec_metrics_window_accumulator.push(sample);
                }
                ticks_in_current_window += 1;
                if ticks_in_current_window == SAMPLES_PER_RESOURCE_USAGE_WINDOW {
                    ticks_in_current_window = 0;
                    usage_window_start_instant = Instant::now();
                    if let Some(window) =
                        exec_metrics_window_accumulator.take_window(FULL_RESOURCE_USAGE_WINDOW_MS)
                    {
                        let _ = outgoing.try_send(AgentToWorker::ResourceUsageWindow(window));
                    }
                }
            }
            changed = progress.changed() => {
                if changed.is_err() {
                    break;
                }
            }
        }
    }

    let window_ms =
        u32::try_from(usage_window_start_instant.elapsed().as_millis()).unwrap_or(u32::MAX);
    if let Some(window) = exec_metrics_window_accumulator.take_window(window_ms) {
        let _ = outgoing.try_send(AgentToWorker::ResourceUsageWindow(window));
    }
}

fn read_system_usage_sample_from_proc_meminfo_and_work_disk_using_cpu_tick_difference(
    previous_cpu_tick_counters: &mut Option<AggregateCpuTickCountersWithOnlineCpuCount>,
) -> Option<OneSecondCpuMemoryDiskUsageSample> {
    let current_cpu_ticks = read_proc_stat_cpu_tick_counters()?;
    let earlier = previous_cpu_tick_counters.replace(current_cpu_ticks)?;
    let meminfo = std::fs::read_to_string("/proc/meminfo").ok()?;
    let (ram_used_bytes, ram_total_bytes) = parse_meminfo_used_and_total_bytes(&meminfo)?;
    let (disk_used_bytes, disk_total_bytes) = read_work_filesystem_used_and_total_bytes();
    Some(OneSecondCpuMemoryDiskUsageSample {
        cpus_in_use: current_cpu_ticks.average_busy_cpu_count_since_earlier_reading(&earlier),
        ram_used_bytes,
        ram_total_bytes,
        disk_used_bytes,
        disk_total_bytes,
    })
}

fn read_proc_stat_cpu_tick_counters() -> Option<AggregateCpuTickCountersWithOnlineCpuCount> {
    parse_cpu_tick_counters_from_proc_stat(&std::fs::read_to_string("/proc/stat").ok()?)
}

// Unreadable work disks report zero instead of discarding the sample. CPU and RAM still reach the worker.
fn read_work_filesystem_used_and_total_bytes() -> (u64, u64) {
    statvfs(WORK_DIR)
        .map(|work_filesystem_statvfs| {
            let fragment_size = work_filesystem_statvfs.fragment_size();
            let blocks = work_filesystem_statvfs.blocks();
            let blocks_free = work_filesystem_statvfs.blocks_free();
            (
                blocks.saturating_sub(blocks_free) * fragment_size,
                blocks * fragment_size,
            )
        })
        .unwrap_or_default()
}

fn parse_cpu_tick_counters_from_proc_stat(
    stat: &str,
) -> Option<AggregateCpuTickCountersWithOnlineCpuCount> {
    let aggregate_cpu_tick_counters_text =
        stat.lines().find_map(|line| line.strip_prefix("cpu "))?;
    let aggregate_cpu_tick_fields = aggregate_cpu_tick_counters_text
        .split_whitespace()
        .take(8)
        .map(str::parse)
        .collect::<Result<Vec<u64>, _>>()
        .ok()?;
    let &[user, nice, system, idle, iowait, irq, softirq, steal] =
        aggregate_cpu_tick_fields.as_slice()
    else {
        return None;
    };
    let cpu_line_count = stat
        .lines()
        .filter(|line| {
            line.strip_prefix("cpu")
                .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()))
        })
        .count();
    let busy = user + nice + system + irq + softirq + steal;
    Some(AggregateCpuTickCountersWithOnlineCpuCount {
        busy,
        total: busy + idle + iowait,
        cpus: u32::try_from(cpu_line_count).ok()?,
    })
}

fn parse_meminfo_used_and_total_bytes(meminfo: &str) -> Option<(u64, u64)> {
    let total = read_meminfo_bytes_for_key(meminfo, "MemTotal")?;
    let available = read_meminfo_bytes_for_key(meminfo, "MemAvailable")?;
    Some((total.saturating_sub(available), total))
}

fn read_meminfo_bytes_for_key(meminfo: &str, key: &str) -> Option<u64> {
    meminfo
        .lines()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))?
        .trim()
        .strip_suffix("kB")?
        .trim()
        .parse::<u64>()
        .ok()
        .map(|kib| kib * 1024)
}

#[cfg(test)]
mod tests {
    use super::{
        AggregateCpuTickCountersWithOnlineCpuCount, ExecMetricsWindowAccumulator,
        OneSecondCpuMemoryDiskUsageSample, parse_cpu_tick_counters_from_proc_stat,
        parse_meminfo_used_and_total_bytes,
    };

    fn build_one_second_usage_sample_from_cpus_in_use_and_ram_used_bytes(
        cpus_in_use: f32,
        ram_used_bytes: u64,
    ) -> OneSecondCpuMemoryDiskUsageSample {
        OneSecondCpuMemoryDiskUsageSample {
            cpus_in_use,
            ram_used_bytes,
            ram_total_bytes: 1000,
            disk_used_bytes: 10,
            disk_total_bytes: 100,
        }
    }

    #[test]
    fn parses_the_aggregate_cpu_line_and_counts_online_cpus() {
        let stat = "cpu  100 0 50 800 50 0 0 0 0 0\ncpu0 50 0 25 400 25 0 0 0 0 0\ncpu1 50 0 25 400 25 0 0 0 0 0\nintr 0\n";

        assert_eq!(
            parse_cpu_tick_counters_from_proc_stat(stat),
            Some(AggregateCpuTickCountersWithOnlineCpuCount {
                busy: 150,
                total: 1000,
                cpus: 2,
            })
        );
    }

    #[test]
    fn used_memory_excludes_available_memory() {
        let meminfo = "MemTotal:           1000 kB\nMemFree:             100 kB\nMemAvailable:        400 kB\n";

        assert_eq!(
            parse_meminfo_used_and_total_bytes(meminfo),
            Some((600 * 1024, 1000 * 1024))
        );
    }

    #[test]
    fn malformed_proc_text_yields_none() {
        assert_eq!(parse_cpu_tick_counters_from_proc_stat(""), None);
        assert_eq!(parse_meminfo_used_and_total_bytes(""), None);
    }

    #[test]
    fn a_window_reports_average_and_peak() {
        let mut exec_metrics_window_accumulator = ExecMetricsWindowAccumulator::new();
        assert_eq!(exec_metrics_window_accumulator.take_window(10_000), None);

        for (cpus_in_use, ram_used_bytes) in [(0.5, 100), (1.5, 300), (1.0, 200)] {
            exec_metrics_window_accumulator.push(
                build_one_second_usage_sample_from_cpus_in_use_and_ram_used_bytes(
                    cpus_in_use,
                    ram_used_bytes,
                ),
            );
        }
        let first = exec_metrics_window_accumulator
            .take_window(10_000)
            .expect("first window");
        assert_eq!(first.sequence, 0);
        assert_eq!(first.window_ms, 10_000);
        assert_eq!(first.cpu_avg, 1.0);
        assert_eq!(first.cpu_max, 1.5);
        assert_eq!(first.ram_avg_bytes, 200);
        assert_eq!(first.ram_max_bytes, 300);
        assert_eq!(first.ram_total_bytes, 1000);
        assert_eq!(first.disk_max_bytes, 10);
        assert_eq!(first.disk_total_bytes, 100);

        exec_metrics_window_accumulator
            .push(build_one_second_usage_sample_from_cpus_in_use_and_ram_used_bytes(1.0, 100));
        let second = exec_metrics_window_accumulator
            .take_window(4_000)
            .expect("second window");
        assert_eq!(second.sequence, 1);
        assert_eq!(second.window_ms, 4_000);
    }
}
