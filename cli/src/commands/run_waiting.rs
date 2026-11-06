/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::commands::run::has_finished;
use connector::api::{DeviceRequest, ExecJobSummary, ExecUnmet, GuestSize, WaitingReason};
use std::collections::HashMap;

#[derive(Default)]
pub(crate) struct WaitingLines {
    shown: HashMap<String, String>,
}

impl WaitingLines {
    pub(crate) fn changed(&mut self, jobs: &[ExecJobSummary]) -> Vec<String> {
        jobs.iter()
            .filter_map(|job| {
                let line = waiting_line(job);
                let id = job.id.to_string();
                match self.shown.get(&id) == Some(&line) {
                    true => None,
                    false => {
                        self.shown.insert(id, line.clone());
                        Some(line)
                    }
                }
            })
            .collect()
    }
}

fn waiting_line(job: &ExecJobSummary) -> String {
    match job.waiting_reason.as_ref().and_then(reason_text) {
        Some(reason) => format!("{}: {}, {reason}", job.entry_point, job.status),
        None => format!("{}: {}", job.entry_point, job.status),
    }
}

pub(crate) fn ended_before_start(jobs: &[ExecJobSummary]) -> Option<String> {
    let never_started = |job: &ExecJobSummary| has_finished(job.status) && job.started_at.is_none();
    if jobs.is_empty() || !jobs.iter().all(never_started) {
        return None;
    }
    let lines: Vec<String> = jobs
        .iter()
        .map(
            |job| match job.message.as_deref().filter(|m| !m.is_empty()) {
                Some(message) => format!("{} did not start: {message}", job.entry_point),
                None => format!("{} did not start: {}.", job.entry_point, job.status),
            },
        )
        .collect();
    Some(lines.join("\n"))
}

fn reason_text(reason: &WaitingReason) -> Option<String> {
    match reason {
        WaitingReason::EvalWorkers { capability, .. } => {
            Some(format!("waiting for a worker with {capability}"))
        }
        WaitingReason::ExecWorkers {
            unmet,
            matching_workers,
            ..
        } => {
            let parts = unmet_parts(unmet);
            Some(match (parts.is_empty(), *matching_workers) {
                (false, _) => format!("needs {}", parts.join(", ")),
                (true, 0) => "waiting for a matching worker".to_owned(),
                (true, 1) => "waiting for free devices on 1 worker".to_owned(),
                (true, workers) => format!("waiting for free devices on {workers} workers"),
            })
        }
        WaitingReason::ManualStart => Some("waits for a manual start".to_owned()),
        _ => None,
    }
}

fn unmet_parts(unmet: &ExecUnmet) -> Vec<String> {
    let mut parts = Vec::new();
    parts.extend(
        unmet
            .architecture
            .iter()
            .map(|a| format!("architecture {a}")),
    );
    parts.extend(unmet.labels.iter().map(|label| format!("label {label}")));
    parts.extend(unmet.devices.iter().map(device_label));
    parts.extend(unmet.guest.iter().flat_map(guest_parts));
    parts.extend(unmet.mounts.iter().map(|mount| format!("mount {mount}")));
    if unmet.nodes > 1 {
        parts.push(format!("{} workers in one zone", unmet.nodes));
    }
    if unmet.custom_image {
        parts.push("a worker that allows custom images".to_owned());
    }
    parts.extend(
        unmet
            .secrets
            .iter()
            .map(|name| format!("worker secret {name}")),
    );
    if unmet.network == Some(true) {
        parts.push("a worker that allows network".to_owned());
    }
    if unmet.ssh == Some(true) {
        parts.push("a worker that allows SSH".to_owned());
    }
    parts.extend(unmet.ports.iter().map(|port| format!("port {port}")));
    parts
}

fn device_label(device: &DeviceRequest) -> String {
    match device.count {
        1 => device.name.clone(),
        count => format!("{}:{count}", device.name),
    }
}

fn guest_parts(size: &GuestSize) -> Vec<String> {
    let mut parts = Vec::new();
    match size.cpus {
        0 => {}
        1 => parts.push("1 CPU".to_owned()),
        cpus => parts.push(format!("{cpus} CPUs")),
    }
    if size.memory_mb > 0 {
        parts.push(format!("{} MB memory", size.memory_mb));
    }
    if size.disk_mb > 0 {
        parts.push(format!("{} MB disk", size.disk_mb));
    }
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    const JOB: &str = "0195b2a4-7c4e-7000-8000-000000000001";

    fn job(fields: Value) -> ExecJobSummary {
        let mut summary = json!({
            "id": JOB,
            "entry_point": "apps.x86_64-linux.frontend",
            "status": "queued",
            "nodes": 1,
            "labels": [],
            "devices": [],
            "autostart": true,
            "keep_vm_running": false,
        });
        summary
            .as_object_mut()
            .expect("object")
            .extend(fields.as_object().expect("object").clone());
        serde_json::from_value(summary).expect("a valid job summary")
    }

    fn missing_ssh() -> Value {
        json!({
            "waiting_reason": {
                "kind": "exec_workers",
                "unmet": {"labels": [], "devices": [], "mounts": [], "nodes": 1, "custom_image": false, "ssh": true},
                "connected_workers": 2,
                "matching_workers": 0,
            }
        })
    }

    #[test]
    fn a_waiting_job_names_its_status_and_the_missing_worker_feature() {
        assert_eq!(
            WaitingLines::default().changed(&[job(missing_ssh())]),
            ["apps.x86_64-linux.frontend: queued, needs a worker that allows SSH"]
        );
    }

    #[test]
    fn a_line_repeats_only_after_the_job_changes() {
        let mut lines = WaitingLines::default();
        lines.changed(&[job(missing_ssh())]);
        assert!(lines.changed(&[job(missing_ssh())]).is_empty());
        assert_eq!(
            lines.changed(&[job(json!({"status": "starting"}))]),
            ["apps.x86_64-linux.frontend: starting"]
        );
    }

    #[test]
    fn a_job_aborted_before_it_started_reports_why() {
        let aborted = job(json!({
            "status": "failed",
            "message": "No connected worker offers ssh. Enable waiting for workers on the task to wait instead.",
        }));
        assert_eq!(
            ended_before_start(&[aborted]).as_deref(),
            Some(
                "apps.x86_64-linux.frontend did not start: No connected worker offers ssh. Enable waiting for workers on the task to wait instead."
            )
        );
    }

    #[test]
    fn a_job_that_ran_or_still_waits_is_no_start_failure() {
        let ran = job(json!({"status": "failed", "started_at": "2026-11-06T12:00:00"}));
        assert_eq!(ended_before_start(&[ran]), None);
        assert_eq!(ended_before_start(&[job(missing_ssh())]), None);
    }
}
