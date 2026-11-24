/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_api_types::{EvalCapability, ExecUnmet, UnmetRequirement, WaitingReason};

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_api_types::DeviceRequest;

    #[test]
    fn a_workers_row_round_trips() {
        let reason = WaitingReason::Workers {
            unmet: Vec::new(),
            connected_workers: 3,
            available_architectures: vec!["x86_64-linux".to_owned()],
        };
        let decoded = WaitingReason::from_json(&reason.to_json()).expect("row decodes");
        assert_eq!(decoded, reason);
    }

    #[test]
    fn worker_gap_ignores_counts_and_order() {
        let unmet = |arch: &str, features: &[&str], build_count| UnmetRequirement {
            architecture: arch.to_owned(),
            required_features: features.iter().map(|f| f.to_string()).collect(),
            build_count,
        };
        let a = WaitingReason::workers(
            vec![
                unmet("x86_64-linux", &["kvm", "big"], 1),
                unmet("aarch64-linux", &[], 2),
            ],
            1,
            vec![],
        );
        let b = WaitingReason::workers(
            vec![
                unmet("aarch64-linux", &[], 7),
                unmet("x86_64-linux", &["big", "kvm"], 3),
            ],
            4,
            vec![],
        );
        assert_eq!(a.worker_gap(), b.worker_gap());
        assert_eq!(WaitingReason::workers(vec![], 1, vec![]).worker_gap(), None);
        assert_eq!(WaitingReason::Draining.worker_gap(), None);
    }

    #[test]
    fn an_exec_row_stored_before_ssh_network_and_ports_still_decodes() {
        let stored = serde_json::json!({
            "kind": "exec_workers",
            "unmet": {"labels": ["3d-printer"], "devices": [], "mounts": [], "nodes": 0, "custom_image": false},
            "connected_workers": 1,
            "matching_workers": 0,
        });
        let Some(WaitingReason::ExecWorkers { unmet, .. }) = WaitingReason::from_json(&stored)
        else {
            panic!("the stored row decodes");
        };
        assert_eq!(
            (unmet.ssh, unmet.network, unmet.ports),
            (false, false, vec![])
        );
    }

    #[test]
    fn the_wait_reason_names_missing_ssh_network_and_ports() {
        let unmet = ExecUnmet {
            ssh: true,
            network: true,
            ports: vec![8080, 22],
            ..ExecUnmet::default()
        };
        assert_eq!(unmet.to_string(), "network,ssh,port:22,port:8080");
        assert!(!unmet.is_empty());
    }

    #[test]
    fn an_exec_workers_row_round_trips() {
        let reason = WaitingReason::ExecWorkers {
            unmet: ExecUnmet {
                labels: vec!["3d-printer".into()],
                devices: vec![DeviceRequest {
                    name: "gpu:h100".into(),
                    count: 8,
                }],
                ..Default::default()
            },
            connected_workers: 2,
            matching_workers: 0,
        };
        assert_eq!(WaitingReason::from_json(&reason.to_json()), Some(reason));
        assert_eq!(
            WaitingReason::eval_workers(EvalCapability::Exec, 1)
                .worker_gap()
                .as_deref(),
            Some("exec")
        );
    }
}
