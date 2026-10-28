/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, BTreeSet};

use gradient_types::secret::SecretString;
use gradient_vm::PluggedDevices;
use gradient_wire::guest::ContextLayers;
use gradient_wire::messages::{ClusterAddress, ClusterPeer};
use gradient_wire::types::ExecJob;
use serde::Serialize;

const SINGLE_NODE_MASTER: &str = "127.0.0.1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClusterContext {
    pub node_index: u32,
    pub node_count: u32,
    pub master_addr: String,
    pub master_port: u16,
    pub peers: Vec<PeerAddress>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PeerAddress {
    pub index: u32,
    pub address: String,
    pub port: u16,
}

pub fn cluster_context(
    me: &ClusterAddress,
    roster: &[ClusterPeer],
    master_port: Option<u16>,
) -> Result<ClusterContext, String> {
    let mut nodes: Vec<&ClusterPeer> = roster.iter().filter(|peer| peer.role == me.role).collect();
    nodes.sort_by_key(|peer| peer.index);
    let master_addr = nodes
        .iter()
        .find(|peer| peer.index == 0)
        .and_then(|peer| peer.endpoint.clone())
        .ok_or("the cluster roster names no master address")?;
    let master_port = master_port.ok_or("the server sent no master port")?;
    let peers = nodes
        .iter()
        .map(|peer| peer_address(peer, master_port))
        .collect::<Result<Vec<_>, _>>()?;
    refuse_duplicate_addresses(&peers)?;
    Ok(ClusterContext {
        node_index: me.index,
        node_count: u32::try_from(peers.len()).unwrap_or(u32::MAX),
        master_addr,
        master_port,
        peers,
    })
}

fn peer_address(peer: &ClusterPeer, port: u16) -> Result<PeerAddress, String> {
    let address = peer.endpoint.clone().ok_or_else(|| {
        format!(
            "the cluster roster names no address for node {}",
            peer.index
        )
    })?;
    Ok(PeerAddress {
        index: peer.index,
        address,
        port,
    })
}

fn refuse_duplicate_addresses(peers: &[PeerAddress]) -> Result<(), String> {
    let mut seen = BTreeSet::new();
    match peers
        .iter()
        .find(|peer| !seen.insert(peer.address.as_str()))
    {
        Some(peer) => Err(format!(
            "two nodes have the guest address {}; give every exec worker its own exec.subnet",
            peer.address
        )),
        None => Ok(()),
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "one value per context source, each layer reads its own"
)]
pub fn context_layers(
    job: &ExecJob,
    token: &SecretString,
    worker_id: &str,
    zone: Option<&str>,
    plugged: &PluggedDevices,
    mounts: &[String],
    address: Option<&str>,
    cluster: Option<&ClusterContext>,
) -> ContextLayers {
    let cluster = match cluster {
        Some(cluster) => cluster_layer(job.attempt, cluster),
        None => single_node_layer(job.attempt, address),
    };
    ContextLayers {
        worker: worker_layer(worker_id, zone, plugged, mounts),
        cluster,
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

fn cluster_layer(attempt: u32, cluster: &ClusterContext) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("node_index".to_owned(), cluster.node_index.to_string()),
        ("node_count".to_owned(), cluster.node_count.to_string()),
        ("master_addr".to_owned(), cluster.master_addr.clone()),
        ("master_port".to_owned(), cluster.master_port.to_string()),
        (
            "peers".to_owned(),
            serde_json::to_string(&cluster.peers).unwrap_or_default(),
        ),
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
        (
            "parent_job".to_owned(),
            context.parent_job.clone().unwrap_or_default(),
        ),
        ("project_name".to_owned(), context.project_name.clone()),
        ("task_name".to_owned(), context.task_name.clone()),
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
            &token(),
            "worker-1",
            Some("fra1"),
            plugged,
            &["datasets".into()],
            None,
            None,
        )
    }

    fn token() -> SecretString {
        SecretString::new("t0k".into())
    }

    fn two_node_job() -> ExecJob {
        let mut job = job(JobContext {
            master_port: Some(29500),
            ..Default::default()
        });
        job.requirement.nodes = 2;
        job
    }

    fn peer(index: u32, endpoint: &str) -> ClusterPeer {
        ClusterPeer {
            role: "node".into(),
            index,
            worker: format!("w{index}"),
            zone: Some("lab".into()),
            endpoint: Some(endpoint.into()),
        }
    }

    #[test]
    fn node_one_learns_the_master_and_every_peer() {
        let me = ClusterAddress {
            role: "node".into(),
            index: 1,
        };
        let ctx = cluster_context(
            &me,
            &[peer(1, "10.234.0.2"), peer(0, "10.233.0.2")],
            Some(29500),
        )
        .unwrap();
        assert_eq!(
            (
                ctx.node_index,
                ctx.node_count,
                ctx.master_addr.as_str(),
                ctx.master_port
            ),
            (1, 2, "10.233.0.2", 29500)
        );
        assert_eq!(
            serde_json::to_value(&ctx.peers).unwrap(),
            serde_json::json!([{"index":0,"address":"10.233.0.2","port":29500},{"index":1,"address":"10.234.0.2","port":29500}])
        );
    }

    #[test]
    fn duplicate_peer_addresses_are_refused() {
        let me = ClusterAddress {
            role: "node".into(),
            index: 0,
        };
        let err = cluster_context(
            &me,
            &[peer(0, "10.233.0.2"), peer(1, "10.233.0.2")],
            Some(29500),
        )
        .unwrap_err();
        assert!(
            err.contains("10.233.0.2") && err.contains("exec.subnet"),
            "{err}"
        );
    }

    #[test]
    fn a_roster_without_master_or_port_is_refused() {
        let me = ClusterAddress {
            role: "node".into(),
            index: 1,
        };
        assert_eq!(
            cluster_context(&me, &[peer(1, "10.234.0.2")], Some(29500)).unwrap_err(),
            "the cluster roster names no master address"
        );
        assert_eq!(
            cluster_context(&me, &[peer(0, "10.233.0.2"), peer(1, "10.234.0.2")], None)
                .unwrap_err(),
            "the server sent no master port"
        );
    }

    #[test]
    fn the_cluster_layer_carries_the_spec_keys() {
        let me = ClusterAddress {
            role: "node".into(),
            index: 0,
        };
        let ctx = cluster_context(
            &me,
            &[peer(0, "10.233.0.2"), peer(1, "10.234.0.2")],
            Some(29500),
        )
        .unwrap();
        let layers = context_layers(
            &two_node_job(),
            &token(),
            "w0",
            Some("lab"),
            &PluggedDevices::default(),
            &[],
            Some("10.233.0.2"),
            Some(&ctx),
        );
        assert_eq!(
            layers.cluster.get("master_addr").map(String::as_str),
            Some("10.233.0.2")
        );
        assert_eq!(
            layers.cluster.get("node_count").map(String::as_str),
            Some("2")
        );
        assert_eq!(
            layers.cluster.get("master_port").map(String::as_str),
            Some("29500")
        );
        assert!(layers.cluster.contains_key("peers") && layers.cluster.contains_key("attempt"));
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
    fn the_parent_job_and_names_reach_the_job_layer() {
        let context = JobContext {
            parent_job: Some("j0".into()),
            project_name: "infra".into(),
            task_name: "master".into(),
            ..Default::default()
        };
        let without_parent = layers(&job(JobContext::default()), &PluggedDevices::default()).job;

        let job_layer = layers(&job(context), &PluggedDevices::default()).job;

        assert_eq!(job_layer["parent_job"], "j0");
        assert_eq!(job_layer["project_name"], "infra");
        assert_eq!(job_layer["task_name"], "master");
        assert_eq!(without_parent["parent_job"], "");
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
