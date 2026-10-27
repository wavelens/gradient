/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;

use gradient_types::secret::SecretString;
use gradient_vm::PluggedDevices;
use gradient_wire::guest::ContextLayers;
use gradient_wire::types::ExecJob;

const SINGLE_NODE_MASTER: &str = "127.0.0.1";

pub fn context_layers(
    job: &ExecJob,
    token: &SecretString,
    worker_id: &str,
    zone: Option<&str>,
    plugged: &PluggedDevices,
    mounts: &[String],
    address: Option<&str>,
) -> ContextLayers {
    ContextLayers {
        worker: worker_layer(worker_id, zone, plugged, mounts),
        cluster: single_node_layer(job.attempt, address),
        job: job_layer(job, token),
    }
}

fn worker_layer(
    worker_id: &str,
    zone: Option<&str>,
    plugged: &PluggedDevices,
    mounts: &[String],
) -> BTreeMap<String, String> {
    let mut layer = BTreeMap::from([
        ("worker".to_owned(), worker_id.to_owned()),
        ("zone".to_owned(), zone.unwrap_or_default().to_owned()),
        (
            "devices".to_owned(),
            serde_json::to_string(&plugged.guest_devices).unwrap_or_default(),
        ),
        ("gpu_indices".to_owned(), gpu_indices(plugged)),
    ]);
    layer.extend(
        mounts
            .iter()
            .map(|name| (format!("mount_{name}"), format!("/mnt/{name}"))),
    );
    layer
}

fn gpu_indices(plugged: &PluggedDevices) -> String {
    let gpus: usize = plugged
        .guest_devices
        .iter()
        .filter(|(class, _)| class.starts_with("gpu"))
        .map(|(_, handles)| handles.len())
        .sum();
    (0..gpus)
        .map(|index| index.to_string())
        .collect::<Vec<_>>()
        .join(",")
}

fn single_node_layer(attempt: u32, address: Option<&str>) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("node_index".to_owned(), "0".to_owned()),
        ("node_count".to_owned(), "1".to_owned()),
        (
            "master_addr".to_owned(),
            address.unwrap_or(SINGLE_NODE_MASTER).to_owned(),
        ),
        ("master_port".to_owned(), String::new()),
        ("peers".to_owned(), "[]".to_owned()),
        ("attempt".to_owned(), attempt.to_string()),
    ])
}

fn job_layer(job: &ExecJob, token: &SecretString) -> BTreeMap<String, String> {
    let context = &job.context;
    let phase = context
        .last_phase
        .as_ref()
        .map(|phase| phase.name.clone())
        .unwrap_or_default();
    BTreeMap::from([
        ("job".to_owned(), context.job.clone()),
        ("evaluation".to_owned(), context.evaluation.clone()),
        ("task".to_owned(), context.task.clone()),
        ("project".to_owned(), context.project.clone()),
        ("commit".to_owned(), context.commit.clone()),
        ("entry_point".to_owned(), context.entry_point.clone()),
        ("phase".to_owned(), phase),
        ("api".to_owned(), context.api_url.clone()),
        ("token".to_owned(), token.expose().to_owned()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_wire::types::{ExecJob, JobContext, Phase};

    fn job(context: JobContext) -> ExecJob {
        ExecJob {
            program: "/nix/store/aaaa-train/bin/train".into(),
            args: vec![],
            env: Default::default(),
            image: None,
            requirement: Default::default(),
            context,
            secrets: vec![],
            keep_running: false,
            timeout_secs: None,
            max_silent_secs: None,
            attempt: 2,
        }
    }

    fn layers(job: &ExecJob, plugged: &PluggedDevices) -> ContextLayers {
        context_layers(
            job,
            &SecretString::new("t0k".into()),
            "worker-1",
            Some("fra1"),
            plugged,
            &["datasets".into()],
            None,
        )
    }

    #[test]
    fn two_gpus_get_indices_and_device_handles() {
        let plugged = PluggedDevices {
            guest_devices: [(
                "gpu:h100".to_owned(),
                vec!["0000:00:05.0".to_owned(), "0000:00:06.0".to_owned()],
            )]
            .into(),
            ..Default::default()
        };

        let worker = layers(&job(JobContext::default()), &plugged).worker;

        assert_eq!(worker["gpu_indices"], "0,1");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&worker["devices"]).unwrap(),
            serde_json::json!({ "gpu:h100": ["0000:00:05.0", "0000:00:06.0"] })
        );
        assert_eq!(worker["mount_datasets"], "/mnt/datasets");
        assert_eq!(worker["zone"], "fra1");
    }

    #[test]
    fn the_last_phase_name_reaches_the_job_layer() {
        let context = JobContext {
            last_phase: Some(Phase {
                name: "epoch-3".into(),
                payload: b"{}".to_vec(),
            }),
            ..Default::default()
        };

        let job_layer = layers(&job(context), &PluggedDevices::default()).job;

        assert_eq!(job_layer["phase"], "epoch-3");
    }

    #[test]
    fn the_token_comes_from_the_wrapped_secret() {
        let context = JobContext {
            api_url: "https://gradient.example.com/api".into(),
            token: String::new(),
            ..Default::default()
        };

        let job_layer = layers(&job(context), &PluggedDevices::default()).job;

        assert_eq!(job_layer["token"], "t0k");
        assert_eq!(job_layer["api"], "https://gradient.example.com/api");
    }

    #[test]
    fn a_single_node_is_its_own_master() {
        let cluster = layers(&job(JobContext::default()), &PluggedDevices::default()).cluster;

        assert_eq!(cluster["node_index"], "0");
        assert_eq!(cluster["node_count"], "1");
        assert_eq!(cluster["master_addr"], "127.0.0.1");
        assert_eq!(cluster["peers"], "[]");
        assert_eq!(cluster["attempt"], "2");
    }
}
