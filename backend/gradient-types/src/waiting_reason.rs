/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_wire::types::DeviceRequest;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum EvalCapability {
    Network,
    Secrets,
    Eval,
    Exec,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitingReason {
    Workers {
        unmet: Vec<UnmetRequirement>,
        connected_workers: u32,
        available_architectures: Vec<String>,
    },
    /// `connected_workers` is the whole connected pool. It can be above zero while only
    /// build-only workers are online and an eval or network worker is missing.
    EvalWorkers {
        capability: EvalCapability,
        connected_workers: u32,
    },
    Approval {
        pr_number: u64,
        pr_author: String,
    },
    NoCache,
    CacheStorageFull,
    Draining,
    GraphStuck {
        pending_shared_builds: u32,
    },
    UpstreamCaches {
        pending_shared_builds: u32,
        failing_upstream_caches: Vec<String>,
    },
    ExecWorkers {
        unmet: ExecUnmet,
        connected_workers: u32,
        matching_workers: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct ExecUnmet {
    pub labels: Vec<String>,
    pub devices: Vec<DeviceRequest>,
    pub mounts: Vec<String>,
    pub nodes: u32,
    pub custom_image: bool,
}

impl ExecUnmet {
    fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

impl std::fmt::Display for ExecUnmet {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut parts: Vec<String> = self.labels.iter().map(|l| format!("label:{l}")).collect();
        parts.sort();
        let mut devices: Vec<String> = self.devices.iter().map(|d| format!("device:{d}")).collect();
        devices.sort();
        parts.extend(devices);
        let mut mounts: Vec<String> = self.mounts.iter().map(|m| format!("mount:{m}")).collect();
        mounts.sort();
        parts.extend(mounts);
        if self.nodes > 0 {
            parts.push(format!("nodes:{}", self.nodes));
        }
        if self.custom_image {
            parts.push("custom-image".to_owned());
        }
        f.write_str(&parts.join(","))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
pub struct UnmetRequirement {
    pub architecture: String,
    pub required_features: Vec<String>,
    pub build_count: u32,
}

impl std::fmt::Display for UnmetRequirement {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.architecture)?;
        if !self.required_features.is_empty() {
            write!(f, " with features {}", self.required_features.join(", "))?;
        }

        write!(f, " ({} builds)", self.build_count)
    }
}

impl WaitingReason {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        serde_json::from_value::<Self>(value.clone()).ok()
    }

    pub fn workers(
        unmet: Vec<UnmetRequirement>,
        connected_workers: u32,
        available_architectures: Vec<String>,
    ) -> Self {
        Self::Workers {
            unmet,
            connected_workers,
            available_architectures,
        }
    }

    pub fn approval(pr_number: u64, pr_author: impl Into<String>) -> Self {
        Self::Approval {
            pr_number,
            pr_author: pr_author.into(),
        }
    }

    pub fn eval_workers(capability: EvalCapability, connected_workers: u32) -> Self {
        Self::EvalWorkers {
            capability,
            connected_workers,
        }
    }

    pub fn graph_stuck(pending_shared_builds: u32) -> Self {
        Self::GraphStuck {
            pending_shared_builds,
        }
    }

    pub fn upstream_caches(
        pending_shared_builds: u32,
        failing_upstream_caches: Vec<String>,
    ) -> Self {
        Self::UpstreamCaches {
            pending_shared_builds,
            failing_upstream_caches,
        }
    }

    pub fn worker_gap(&self) -> Option<String> {
        match self {
            Self::Workers { unmet, .. } if !unmet.is_empty() => {
                let mut gaps: Vec<String> = unmet
                    .iter()
                    .map(|u| {
                        let mut features = u.required_features.clone();
                        features.sort();
                        format!("{}[{}]", u.architecture, features.join(","))
                    })
                    .collect();
                gaps.sort();
                gaps.dedup();
                Some(gaps.join(";"))
            }
            Self::EvalWorkers { capability, .. } => Some(match capability {
                EvalCapability::Network => "network".to_owned(),
                EvalCapability::Secrets => "secrets".to_owned(),
                EvalCapability::Eval => "eval".to_owned(),
                EvalCapability::Exec => "exec".to_owned(),
            }),
            Self::ExecWorkers { unmet, .. } if !unmet.is_empty() => Some(format!("exec[{unmet}]")),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
