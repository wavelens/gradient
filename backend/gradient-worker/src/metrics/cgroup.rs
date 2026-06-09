/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Per-build resource snapshot read from a cgroup-v2 directory.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct BuildMetricsRaw {
    pub peak_ram_bytes: Option<u64>,
    pub cpu_usage_usec: Option<u64>,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub oom_killed: bool,
}

pub fn parse_memory_peak(s: &str) -> Option<u64> {
    s.trim().parse().ok()
}

pub fn parse_cpu_usage_usec(s: &str) -> Option<u64> {
    s.lines()
        .find(|l| l.starts_with("usage_usec "))?
        .split_ascii_whitespace()
        .nth(1)?
        .parse()
        .ok()
}

pub fn parse_io_stat(s: &str) -> (u64, u64) {
    s.lines().fold((0, 0), |(rb, wb), line| {
        let mut r = rb;
        let mut w = wb;
        for token in line.split_ascii_whitespace().skip(1) {
            if let Some(v) = token.strip_prefix("rbytes=").and_then(|n| n.parse::<u64>().ok()) {
                r += v;
            } else if let Some(v) = token.strip_prefix("wbytes=").and_then(|n| n.parse::<u64>().ok()) {
                w += v;
            }
        }
        (r, w)
    })
}

pub fn parse_oom_kill(s: &str) -> bool {
    s.lines()
        .find(|l| l.starts_with("oom_kill "))
        .and_then(|l| l.split_ascii_whitespace().nth(1))
        .and_then(|n| n.parse::<u64>().ok())
        .map(|n| n > 0)
        .unwrap_or(false)
}

/// Read cgroup-v2 files under `dir`. Returns `None` if `dir` does not exist;
/// missing individual files degrade to `None`/`0`/`false`.
pub fn read_build_cgroup(dir: &std::path::Path) -> Option<BuildMetricsRaw> {
    if !dir.exists() {
        return None;
    }
    let read = |name| std::fs::read_to_string(dir.join(name)).ok();
    let io = read("io.stat").map(|s| parse_io_stat(&s)).unwrap_or((0, 0));
    Some(BuildMetricsRaw {
        peak_ram_bytes: read("memory.peak").and_then(|s| parse_memory_peak(&s)),
        cpu_usage_usec: read("cpu.stat").and_then(|s| parse_cpu_usage_usec(&s)),
        disk_read_bytes: io.0,
        disk_write_bytes: io.1,
        oom_killed: read("memory.events").map(|s| parse_oom_kill(&s)).unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_stat_usage() {
        let s = "usage_usec 1234567\nuser_usec 1000000\nsystem_usec 234567\n";
        assert_eq!(parse_cpu_usage_usec(s), Some(1_234_567));
    }

    #[test]
    fn io_stat_sums_devices() {
        let s = "8:0 rbytes=1000 wbytes=2000 rios=1 wios=2\n8:16 rbytes=500 wbytes=500\n";
        assert_eq!(parse_io_stat(s), (1500, 2500));
    }

    #[test]
    fn memory_events_oom() {
        assert!(parse_oom_kill("low 0\nhigh 0\noom 1\noom_kill 3\n"));
        assert!(!parse_oom_kill("low 0\noom_kill 0\n"));
    }

    #[test]
    fn memory_peak_value() {
        assert_eq!(parse_memory_peak("4194304\n"), Some(4_194_304));
    }

    #[test]
    fn read_missing_dir_is_none() {
        assert!(read_build_cgroup(std::path::Path::new("/nonexistent/cgroup/xyz")).is_none());
    }

    #[test]
    fn read_build_cgroup_from_tempdir() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::write(p.join("memory.peak"), "4194304\n").unwrap();
        std::fs::write(p.join("cpu.stat"), "usage_usec 1234567\nuser_usec 1000000\n").unwrap();
        std::fs::write(p.join("io.stat"), "8:0 rbytes=1000 wbytes=2000\n").unwrap();
        std::fs::write(p.join("memory.events"), "oom_kill 0\n").unwrap();

        let m = read_build_cgroup(p).unwrap();
        assert_eq!(m.peak_ram_bytes, Some(4_194_304));
        assert_eq!(m.cpu_usage_usec, Some(1_234_567));
        assert_eq!(m.disk_read_bytes, 1000);
        assert_eq!(m.disk_write_bytes, 2000);
        assert!(!m.oom_killed);
    }
}
