/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Structured "why is this evaluation waiting?" payload.
//!
//! Persisted on `evaluation.waiting_reason` (JSON) by the scheduler and the
//! trigger pipeline, returned by `GET /evals/{evaluation}`, rendered by the
//! frontend's waiting panel.
//!
//! Reasons:
//! - `Workers` - no connected worker can satisfy the pending builds' arch /
//!   required-feature combo (build phase; persisted under JSON `kind=workers`).
//! - `EvalWorkers` - the evaluation is still in a pre-build phase (`Fetching`
//!   needs a fetch-capable worker; `Queued`/`EvaluatingFlake`/
//!   `EvaluatingDerivation` need an eval-capable worker) and no connected
//!   worker provides that capability.
//! - `Approval` - pull-request evaluation from a contributor who is not a
//!   forge writer on the repo, gated until a maintainer approves.
//! - `NoCache` - the project's organisation has no active cache configured,
//!   so the build outputs would have nowhere to land.
//! - `CacheStorageFull` - every writable cache for the organisation is within
//!   the headroom threshold of its (or the instance-wide) max-storage limit.
//! - `Draining` - the instance is draining (superuser action): all in-flight
//!   evaluations are parked so the server can be stopped safely. Cleared on the
//!   next startup or when draining is disabled.

use serde::{Deserialize, Serialize};

/// Pre-build capability a stalled evaluation is waiting for a worker to provide.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvalCapability {
    /// `Fetching` evaluations need a worker advertising the `fetch` capability.
    Fetch,
    /// `Queued`/`EvaluatingFlake`/`EvaluatingDerivation` need an `eval` worker.
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
    /// Pre-build stall: no connected worker provides `capability`.
    /// `connected_workers` is the total connected pool size (may be > 0 when
    /// only build-only workers are online and an eval/fetch worker is missing).
    EvalWorkers {
        capability: EvalCapability,
        connected_workers: u32,
    },
    Approval {
        pr_number: u64,
        pr_author: String,
    },
    NoCache,
    /// Every writable cache for the org is within `STORAGE_HEADROOM_BYTES` of
    /// its configured `max_storage_gb` (or the instance-wide limit), so build
    /// outputs would have nowhere to land.
    CacheStorageFull,
    /// The instance is draining: scheduling is paused and this evaluation is
    /// parked until draining is disabled or the server restarts.
    Draining,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnmetRequirement {
    pub architecture: String,
    pub required_features: Vec<String>,
    pub build_count: u32,
}

impl WaitingReason {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Tolerant of legacy rows written before the `kind` discriminator existed
    /// - those decode as `Workers { .. }`.
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn workers_round_trip_carries_kind_tag() {
        let r = WaitingReason::workers(
            vec![UnmetRequirement {
                architecture: "x86_64-linux".into(),
                required_features: vec!["kvm".into()],
                build_count: 2,
            }],
            1,
            vec!["aarch64-linux".into()],
        );
        let v = r.to_json();
        assert_eq!(v["kind"], "workers");
        assert_eq!(WaitingReason::from_json(&v).unwrap(), r);
    }

    #[test]
    fn approval_round_trip() {
        let r = WaitingReason::approval(42, "octocat");
        let v = r.to_json();
        assert_eq!(v["kind"], "approval");
        assert_eq!(v["pr_number"], 42);
        assert_eq!(v["pr_author"], "octocat");
        assert_eq!(WaitingReason::from_json(&v).unwrap(), r);
    }

    #[test]
    fn no_cache_round_trip() {
        let r = WaitingReason::NoCache;
        let v = r.to_json();
        assert_eq!(v["kind"], "no_cache");
        assert_eq!(WaitingReason::from_json(&v).unwrap(), r);
    }

    #[test]
    fn cache_storage_full_round_trip() {
        let r = WaitingReason::CacheStorageFull;
        let v = r.to_json();
        assert_eq!(v["kind"], "cache_storage_full");
        assert_eq!(WaitingReason::from_json(&v).unwrap(), r);
    }

    #[test]
    fn draining_round_trip() {
        let r = WaitingReason::Draining;
        let v = r.to_json();
        assert_eq!(v["kind"], "draining");
        assert_eq!(WaitingReason::from_json(&v).unwrap(), r);
    }

    #[test]
    fn eval_workers_round_trip_carries_capability() {
        for (cap, expected) in [(EvalCapability::Fetch, "fetch"), (EvalCapability::Eval, "eval")] {
            let r = WaitingReason::eval_workers(cap, 2);
            let v = r.to_json();
            assert_eq!(v["kind"], "eval_workers");
            assert_eq!(v["capability"], expected);
            assert_eq!(v["connected_workers"], 2);
            assert_eq!(WaitingReason::from_json(&v).unwrap(), r);
        }
    }

    /// Legacy rows persisted before the `kind` tag existed must still
    /// decode - they all represent the workers-capacity reason.
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
