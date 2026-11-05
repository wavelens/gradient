/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::run::{first_failure, has_finished, has_started, job_exit_code};
use crate::output::Output;
use connector::api::{ExecJobStatus, ExecJobSummary};
use std::io::Write;
use std::process::exit;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const INTERRUPTED_EXIT_CODE: i32 = 130;

struct FollowedJob {
    id: String,
    attr: String,
    printed: usize,
    lines: LinePrefixer,
    finished: bool,
}

pub(crate) async fn follow(
    client: &connector::Client,
    evaluation: &str,
    jobs: Vec<ExecJobSummary>,
    out: Output,
) -> ! {
    let prefixed = jobs.len() > 1;
    let mut followed: Vec<FollowedJob> = jobs
        .iter()
        .map(|job| FollowedJob::new(job, prefixed))
        .collect();
    let mut exit_codes = Vec::new();
    let mut interrupted = std::pin::pin!(tokio::signal::ctrl_c());
    loop {
        let statuses = client
            .exec_jobs()
            .of_evaluation(evaluation)
            .await
            .unwrap_or_else(|e| {
                out.progress(format!("Failed to list exec jobs: {e}"));
                Vec::new()
            });
        for job in followed.iter_mut().filter(|job| !job.finished) {
            let Some(summary) = statuses.iter().find(|s| s.id.to_string() == job.id) else {
                continue;
            };
            if has_started(summary.status) {
                job.print_new_output(client, out).await;
            }
            if has_finished(summary.status) {
                job.finish(summary, prefixed, out);
                exit_codes.push(job_exit_code(summary.status, summary.program_exit_code));
            }
        }
        if followed.iter().all(|job| job.finished) {
            exit(first_failure(&exit_codes));
        }
        tokio::select! {
            () = tokio::time::sleep(POLL_INTERVAL) => {}
            _ = &mut interrupted => abort_unfinished(client, &followed, out).await,
        }
    }
}

async fn abort_unfinished(client: &connector::Client, followed: &[FollowedJob], out: Output) -> ! {
    out.progress("Aborting unfinished jobs...");
    for job in followed.iter().filter(|job| !job.finished) {
        if let Err(e) = client.exec_jobs().abort(&job.id).await {
            out.progress(format!("Failed to abort job {}: {e}", job.id));
        }
    }
    exit(INTERRUPTED_EXIT_CODE)
}

impl FollowedJob {
    fn new(job: &ExecJobSummary, prefixed: bool) -> Self {
        Self {
            id: job.id.to_string(),
            attr: job.entry_point.clone(),
            printed: 0,
            lines: LinePrefixer::new(prefixed.then(|| format!("{} | ", job.entry_point))),
            finished: false,
        }
    }

    async fn print_new_output(&mut self, client: &connector::Client, out: Output) {
        let Ok(log) = client.exec_jobs().log(&self.id).await else {
            return;
        };
        let fresh = match log.get(self.printed..) {
            Some(fresh) => fresh,
            None => {
                out.progress(format!("Logs for {} resumed in a new attempt.", self.attr));
                &log
            }
        };
        if fresh.is_empty() {
            return;
        }
        let mut stdout = std::io::stdout();
        let _ = stdout.write_all(self.lines.prefix(fresh).as_bytes());
        let _ = stdout.flush();
        self.printed = log.len();
    }

    fn finish(&mut self, summary: &ExecJobSummary, prefixed: bool, out: Output) {
        self.finished = true;
        if self.lines.end_line() {
            println!();
        }
        let message = summary.message.as_deref().filter(|m| !m.is_empty());
        match message {
            Some(message) => out.progress(format!(
                "{} exited with status {}: {message}",
                self.attr, summary.status
            )),
            None if prefixed || summary.status != ExecJobStatus::Completed => out.progress(
                format!("{} exited with status {}.", self.attr, summary.status),
            ),
            None => {}
        }
    }
}

struct LinePrefixer {
    prefix: Option<String>,
    mid_line: bool,
}

impl LinePrefixer {
    fn new(prefix: Option<String>) -> Self {
        Self {
            prefix,
            mid_line: false,
        }
    }

    fn prefix(&mut self, text: &str) -> String {
        let mut prefixed = String::with_capacity(text.len());
        for line in text.split_inclusive('\n') {
            if let Some(prefix) = self.prefix.as_deref().filter(|_| !self.mid_line) {
                prefixed.push_str(prefix);
            }
            prefixed.push_str(line);
            self.mid_line = !line.ends_with('\n');
        }
        prefixed
    }

    fn end_line(&mut self) -> bool {
        std::mem::replace(&mut self.mid_line, false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_line_of_several_jobs_carries_the_attribute_even_across_polls() {
        let mut lines = LinePrefixer::new(Some("apps.x.train | ".into()));

        assert_eq!(
            lines.prefix("epoch 1\nepo"),
            "apps.x.train | epoch 1\napps.x.train | epo"
        );
        assert_eq!(lines.prefix("ch 2\n"), "ch 2\n");
        assert!(!lines.end_line());
    }

    #[test]
    fn a_single_job_prints_its_output_unchanged() {
        let mut lines = LinePrefixer::new(None);

        assert_eq!(lines.prefix("a\nb"), "a\nb");
        assert!(lines.end_line());
    }
}
