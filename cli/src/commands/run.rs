/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::build::{BuildParams, failure_message, submit_build_request};
use crate::commands::completion;
use crate::commands::logstream::stream_eval_logs;
use crate::commands::run_forward::{self, PortForward, parse_forward};
use crate::commands::{run_logs, run_terminal};
use crate::input::client_from_config;
use crate::output::{ExitKind, Output};
use crate::tui::watch::eval_is_terminal;
use clap::Args;
use clap_complete::engine::ArgValueCompleter;
use connector::api::{ExecJobStatus, ExecJobSummary, RunOptions};
use std::io::IsTerminal;
use std::time::Duration;

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_TARGET: &str = ".#default";
const LAST_MESSAGES_WAIT: Duration = Duration::from_secs(5);

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Match an attribute like `gradient build` does (default: `packages.<system>.default`)
    target: Option<String>,
    /// Track the job log instead of connecting this terminal to the program
    #[arg(long)]
    no_shell: bool,
    /// Number of CPUs for the guest
    #[arg(long, value_parser = clap::value_parser!(i32).range(1..))]
    cpus: Option<i32>,
    /// Memory for the guest (MB)
    #[arg(long, value_name = "MB", value_parser = clap::value_parser!(i64).range(1..))]
    memory: Option<i64>,
    /// Give the guest internet access
    #[arg(long)]
    network: bool,
    /// Worker labels the job must have (comma-separated)
    #[arg(long, value_delimiter = ',')]
    labels: Vec<String>,
    /// Limits for total time and time without output (seconds or as `30m`). Workers set upper bounds
    #[arg(long, value_parser = parse_timeout)]
    timeout: Option<u64>,
    /// Target system (default: this machine)
    #[arg(long)]
    system: Option<String>,
    #[arg(short, long, add = ArgValueCompleter::new(completion::complete_projects))]
    project: Option<String>,
    /// Connect this terminal to the program for a running job (no upload and no evaluation)
    #[arg(long, value_name = "JOB_ID", conflicts_with_all = ["target", "no_shell", "cpus", "memory", "network", "labels", "timeout", "system", "project"])]
    attach: Option<String>,
    /// Forward a local port to a port in the guest (example: `ssh -L`, repeatable)
    #[arg(short = 'L', long = "forward", value_name = "[LOCAL:]PORT", value_parser = parse_forward)]
    forwards: Vec<PortForward>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunMode {
    Interactive,
    Logs,
}

pub(crate) fn run_mode(wants_terminal: bool, jobs: usize) -> RunMode {
    match wants_terminal && jobs == 1 {
        true => RunMode::Interactive,
        false => RunMode::Logs,
    }
}

pub async fn handle_run(args: RunArgs, out: Output) {
    if let Some(job) = &args.attach {
        let client = client_from_config(out);
        run_terminal::reattach(&client, job, &args.forwards, out).await;
    }

    let wants_terminal = !args.no_shell && !out.is_json() && has_terminal();
    let params = BuildParams {
        target: Some(
            args.target
                .clone()
                .unwrap_or_else(|| DEFAULT_TARGET.to_owned()),
        ),
        system: args.system.clone(),
        overrides: Vec::new(),
        run: Some(run_options(&args, wants_terminal)),
    };
    let (client, started) = submit_build_request(params, args.project, false, out).await;
    let evaluation = started.evaluation.to_string();
    out.progress(format!("Evaluation: {evaluation}"));

    let jobs = follow_builds_until_started(&client, &evaluation, out).await;
    if !args.forwards.is_empty() {
        if jobs.len() > 1 {
            for job in &jobs {
                let _ = client.exec_jobs().abort(&job.id.to_string()).await;
            }
            out.err(
                ExitKind::Api,
                "Each port forward needs exactly one matching job. This run found several.",
            );
        }
        run_forward::start(&client, &jobs[0].id.to_string(), &args.forwards, out).await;
    }
    match run_mode(wants_terminal, jobs.len()) {
        RunMode::Interactive if !has_finished(jobs[0].status) => {
            run_terminal::attach(&client, &jobs[0].id.to_string(), out).await
        }
        RunMode::Interactive | RunMode::Logs => {
            run_logs::follow(&client, &evaluation, jobs, out).await
        }
    }
}

fn has_terminal() -> bool {
    std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
}

fn run_options(args: &RunArgs, interactive: bool) -> RunOptions {
    RunOptions {
        interactive: Some(interactive),
        cpus: args.cpus,
        memory_mb: args.memory,
        network: args.network.then_some(true),
        labels: (!args.labels.is_empty()).then(|| args.labels.clone()),
        timeout_secs: args
            .timeout
            .map(|secs| i64::try_from(secs).unwrap_or(i64::MAX)),
        ports: args
            .forwards
            .iter()
            .map(|forward| i32::from(forward.guest))
            .collect(),
    }
}

async fn follow_builds_until_started(
    client: &connector::Client,
    evaluation: &str,
    out: Output,
) -> Vec<ExecJobSummary> {
    let builds = stream_eval_logs(client, evaluation, out);
    tokio::pin!(builds);
    let mut builds_followed = false;
    let started = tokio::select! {
        started = wait_until_started(client, evaluation) => started,
        () = &mut builds => {
            builds_followed = true;
            wait_until_started(client, evaluation).await
        }
    };
    let failure = match started {
        Ok(jobs) => return jobs,
        Err(failure) => failure,
    };
    if !builds_followed {
        let _ = tokio::time::timeout(LAST_MESSAGES_WAIT, &mut builds).await;
    }
    out.err(ExitKind::Api, failure)
}

async fn wait_until_started(
    client: &connector::Client,
    evaluation: &str,
) -> Result<Vec<ExecJobSummary>, String> {
    loop {
        let jobs = client
            .exec_jobs()
            .of_evaluation(evaluation)
            .await
            .map_err(|e| format!("Failed to list exec jobs: {e}"))?;
        if jobs.is_empty() {
            if let Some(failure) = failure_without_jobs(client, evaluation).await {
                return Err(failure);
            }
        } else if jobs.iter().all(|job| has_started(job.status)) {
            return Ok(jobs);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn failure_without_jobs(client: &connector::Client, evaluation: &str) -> Option<String> {
    let eval = client.evals().get(evaluation).await.ok()?;
    eval_is_terminal(eval.status).then(|| {
        failure_message(eval.status, eval.error.as_deref())
            .unwrap_or_else(|| "The exec job did not start for this evaluation.".to_owned())
    })
}

pub(crate) fn has_started(status: ExecJobStatus) -> bool {
    !matches!(
        status,
        ExecJobStatus::Created | ExecJobStatus::Queued | ExecJobStatus::Starting
    )
}

pub(crate) fn has_finished(status: ExecJobStatus) -> bool {
    matches!(
        status,
        ExecJobStatus::Completed | ExecJobStatus::Failed | ExecJobStatus::Aborted
    )
}

pub(crate) fn job_exit_code(status: ExecJobStatus, program_exit_code: Option<i32>) -> i32 {
    match (program_exit_code, status) {
        (Some(code), _) => code,
        (None, ExecJobStatus::Completed) => 0,
        (None, _) => 1,
    }
}

pub(crate) fn first_failure(exit_codes: &[i32]) -> i32 {
    exit_codes
        .iter()
        .copied()
        .find(|&code| code != 0)
        .unwrap_or(0)
}

pub(crate) fn parse_timeout(raw: &str) -> Result<u64, String> {
    let invalid =
        || format!("timeout \"{raw}\": use seconds or durations like 90s, 30m, 2h, 1h30m");
    let raw = raw.trim();
    if let Ok(seconds) = raw.parse::<u64>() {
        return (seconds > 0).then_some(seconds).ok_or_else(invalid);
    }

    let mut total: u64 = 0;
    let mut digits = String::new();
    for c in raw.chars() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit = unit_seconds(c).ok_or_else(invalid)?;
        let amount: u64 = digits.parse().map_err(|_| invalid())?;
        digits.clear();
        total = amount
            .checked_mul(unit)
            .and_then(|seconds| total.checked_add(seconds))
            .ok_or_else(invalid)?;
    }
    match digits.is_empty() && total > 0 {
        true => Ok(total),
        false => Err(invalid()),
    }
}

fn unit_seconds(unit: char) -> Option<u64> {
    match unit {
        's' => Some(1),
        'm' => Some(60),
        'h' => Some(3600),
        'd' => Some(86400),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_single_job_with_a_terminal_runs_interactively() {
        assert_eq!(run_mode(true, 1), RunMode::Interactive);
        assert_eq!(run_mode(true, 2), RunMode::Logs);
        assert_eq!(run_mode(false, 1), RunMode::Logs);
    }

    #[test]
    fn a_timeout_is_seconds_or_a_duration() {
        assert_eq!(parse_timeout("90"), Ok(90));
        assert_eq!(parse_timeout("90s"), Ok(90));
        assert_eq!(parse_timeout("30m"), Ok(1800));
        assert_eq!(parse_timeout("1h30m"), Ok(5400));
        assert_eq!(parse_timeout("2d"), Ok(172_800));
    }

    #[test]
    fn a_timeout_without_a_positive_amount_or_with_an_unknown_unit_is_refused() {
        for bad in ["", "0", "0m", "m", "10x", "1h30", "-5", "1.5h"] {
            assert!(parse_timeout(bad).is_err(), "{bad:?} must be refused");
        }
    }

    #[test]
    fn the_program_exit_code_wins_and_a_failure_without_one_is_one() {
        assert_eq!(job_exit_code(ExecJobStatus::Failed, Some(3)), 3);
        assert_eq!(job_exit_code(ExecJobStatus::Completed, None), 0);
        assert_eq!(job_exit_code(ExecJobStatus::Aborted, None), 1);
        assert_eq!(job_exit_code(ExecJobStatus::Failed, None), 1);
    }

    #[test]
    fn several_jobs_exit_with_the_first_non_zero_code() {
        assert_eq!(first_failure(&[0, 4, 2]), 4);
        assert_eq!(first_failure(&[0, 0]), 0);
        assert_eq!(first_failure(&[]), 0);
    }
}
