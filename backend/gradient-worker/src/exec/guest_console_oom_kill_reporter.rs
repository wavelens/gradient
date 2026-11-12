/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use gradient_vm::console::CONSOLE_LOG;
use tracing::warn;

use super::job::ExecReport;

const SCANNED_BYTES: u64 = 256 * 1024;
const KERNEL_OUT_OF_MEMORY_SIGNATURES: &[&str] = &[
    "invoked oom-killer:",
    "out of memory: killed process",
    "out of memory and no killable processes",
    "system is deadlocked on memory",
];

pub struct GuestKernelOutOfMemoryKillLogScanner {
    console: PathBuf,
    memory_mb: u64,
    scanned_to: u64,
}

impl GuestKernelOutOfMemoryKillLogScanner {
    pub fn new(guest_dir: &Path, memory_mb: u64) -> Self {
        let console = guest_dir.join(CONSOLE_LOG);
        let scanned_to = std::fs::metadata(&console).map_or(0, |console| console.len());
        Self {
            console,
            memory_mb,
            scanned_to,
        }
    }

    pub fn out_of_memory_message(&self) -> String {
        format!(
            "guest ran out of memory at {} MB. increase `memoryMb` for the job",
            self.memory_mb
        )
    }

    pub async fn log_kernel_out_of_memory_lines_and_return_if_guest_ran_out_of_memory<
        R: ExecReport,
    >(
        &mut self,
        reporter: &mut R,
    ) -> bool {
        let console = match self.read_unscanned() {
            Ok(console) => console,
            Err(e) => {
                warn!(console = %self.console.display(), error = %e, "failed to read guest console");
                return false;
            }
        };
        let lines = select_out_of_memory_kernel_lines_without_timestamp_prefix(&console);
        if lines.is_empty() {
            return false;
        }
        warn!(
            console = %self.console.display(),
            memory_mb = self.memory_mb,
            kernel = ?lines,
            "guest out of memory"
        );
        let notes = lines
            .iter()
            .map(|line| format!("Guest kernel: {line}"))
            .chain([self.out_of_memory_message()]);
        for note in notes {
            if let Err(e) = reporter.note(&note).await {
                warn!(error = %e, "failed to log the guest out-of-memory report");
            }
        }
        true
    }

    fn read_unscanned(&mut self) -> std::io::Result<String> {
        let mut file = std::fs::File::open(&self.console)?;
        let len = file.metadata()?.len();
        let resumed = match self.scanned_to <= len {
            true => self.scanned_to,
            false => 0,
        };
        let from = resumed.max(len.saturating_sub(SCANNED_BYTES));
        file.seek(SeekFrom::Start(from))?;
        let mut unscanned = Vec::new();
        file.read_to_end(&mut unscanned)?;
        self.scanned_to = from + unscanned.len() as u64;
        Ok(String::from_utf8_lossy(&unscanned).into_owned())
    }
}

fn select_out_of_memory_kernel_lines_without_timestamp_prefix(console: &str) -> Vec<&str> {
    console
        .lines()
        .map(|line| {
            let line = line.trim();
            match line
                .strip_prefix('[')
                .and_then(|rest| rest.split_once("] "))
            {
                Some((_, message)) => message.trim_start(),
                None => line,
            }
        })
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            KERNEL_OUT_OF_MEMORY_SIGNATURES
                .iter()
                .any(|signature| lower.contains(signature))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONSOLE: &str = "\
[    1.204331] systemd[1]: Started gradient-guest-agent.service.\r
[   42.118201] python3 invoked oom-killer: gfp_mask=0x140cca(GFP_HIGHUSER_MOVABLE|__GFP_COMP), order=0, oom_score_adj=0\r
[   42.119900] Out of memory: Killed process 812 (python3) total-vm:912344kB, anon-rss:401220kB, file-rss:0kB, shmem-rss:0kB, UID:1000 pgtables:932kB oom_score_adj:0\r
[   43.000001] Kernel panic - not syncing: System is deadlocked on memory\r
";

    #[test]
    fn only_kernel_out_of_memory_kills_are_selected() {
        assert_eq!(
            select_out_of_memory_kernel_lines_without_timestamp_prefix(CONSOLE),
            [
                "python3 invoked oom-killer: gfp_mask=0x140cca(GFP_HIGHUSER_MOVABLE|__GFP_COMP), order=0, oom_score_adj=0",
                "Out of memory: Killed process 812 (python3) total-vm:912344kB, anon-rss:401220kB, file-rss:0kB, shmem-rss:0kB, UID:1000 pgtables:932kB oom_score_adj:0",
                "Kernel panic - not syncing: System is deadlocked on memory",
            ]
        );
    }

    #[test]
    fn a_console_without_out_of_memory_kills_selects_nothing() {
        let console = "[    1.0] systemd[1]: Started gradient-guest-agent.service.\n";
        assert!(select_out_of_memory_kernel_lines_without_timestamp_prefix(console).is_empty());
    }

    #[test]
    fn each_console_line_written_during_the_job_is_scanned_once() {
        let dir = tempfile::tempdir().unwrap();
        let console = dir.path().join(CONSOLE_LOG);
        std::fs::write(&console, "[    0.1] booted before the job\n").unwrap();
        let mut scanner = GuestKernelOutOfMemoryKillLogScanner::new(dir.path(), 500);
        let mut log = std::fs::OpenOptions::new()
            .append(true)
            .open(&console)
            .unwrap();
        std::io::Write::write_all(&mut log, CONSOLE.as_bytes()).unwrap();

        assert_eq!(scanner.read_unscanned().unwrap(), CONSOLE);
        assert_eq!(scanner.read_unscanned().unwrap(), "");
    }
}
