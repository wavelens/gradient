/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalCapability {
    Fetch,
    Eval,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WaitingReason {
    Workers {
        unmet: Vec<UnmetRequirement>,
        connected_workers: u32,
        available_architectures: Vec<String>,
    },
    /// `connected_workers` is the whole connected pool. It can be above zero while only
    /// build-only workers are online and an eval or fetch worker is missing.
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
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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

    /// Legacy rows from before the `kind` discriminator are decoding as `Workers { .. }`.
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        if let Ok(parsed) = serde_json::from_value::<Self>(value.clone()) {
            return Some(parsed);
        }
        if value.is_object() && value.get("kind").is_none() {
            let mut patched = value.clone();
            if let serde_json::Value::Object(ref mut m) = patched {
                m.insert("kind".into(), serde_json::Value::String("workers".into()));
            }
            return serde_json::from_value::<Self>(patched).ok();
        }
        None
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn legacy_untagged_workers_row_decodes() {
        let legacy = serde_json::json!({
            "unmet": [],
            "connected_workers": 3,
            "available_architectures": ["x86_64-linux"],
        });
        let decoded = WaitingReason::from_json(&legacy).expect("legacy row decodes");
        match decoded {
            WaitingReason::Workers {
                connected_workers,
                available_architectures,
                ..
            } => {
                assert_eq!(connected_workers, 3);
                assert_eq!(available_architectures, vec!["x86_64-linux"]);
            }
            other => panic!("expected Workers, got {other:?}"),
        }
    }
}
