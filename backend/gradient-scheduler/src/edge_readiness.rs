/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Incremental edge-readiness tracker (#392).
//!
//! Decides, batch by batch, when a derivation's full set of direct dependency
//! edges is writable - i.e. every dependency now has a row - so the owning
//! build can be promoted `Created → Queued` mid-evaluation. Order-independent:
//! a dependency may be observed before or after its dependents.

use std::collections::{HashMap, HashSet};

use gradient_types::proto::DiscoveredDerivation;

/// Per-evaluation readiness tracker. Cheap, in-memory, single-eval (batches for
/// one eval arrive in order on one connection); the caller guards cross-eval
/// access with a lock.
#[derive(Default)]
pub(crate) struct EdgeReadiness {
    /// drv_paths that now have a derivation row.
    seen: HashSet<String>,
    /// src -> its full direct-dep list, kept until the src becomes ready.
    deps: HashMap<String, Vec<String>>,
    /// src -> deps still missing a row.
    pending: HashMap<String, HashSet<String>>,
    /// dep -> srcs waiting on it.
    waiters: HashMap<String, HashSet<String>>,
}

impl EdgeReadiness {
    /// Feed one batch. Returns every derivation that became edge-complete in
    /// this batch as `(drv_path, dependencies)`; leaves carry an empty dep list.
    /// Each derivation is reported at most once across the tracker's lifetime.
    pub(crate) fn observe(&mut self, batch: &[DiscoveredDerivation]) -> Vec<(String, Vec<String>)> {
        let mut ready: Vec<(String, Vec<String>)> = Vec::new();

        // 1. Mark every drv_path as seen; keep only the first sighting of each so
        //    a re-observed derivation is never registered or reported twice.
        let mut fresh: Vec<&DiscoveredDerivation> = Vec::new();
        for d in batch {
            if self.seen.insert(d.drv_path.clone()) {
                fresh.push(d);
            }
        }

        // 2. Register each fresh source against the post-step-1 seen set, so deps
        //    that arrived in this same batch already count as satisfied.
        for d in &fresh {
            if d.dependencies.is_empty() {
                ready.push((d.drv_path.clone(), Vec::new()));
                continue;
            }

            let missing: HashSet<String> = d
                .dependencies
                .iter()
                .filter(|dep| !self.seen.contains(*dep))
                .cloned()
                .collect();

            if missing.is_empty() {
                ready.push((d.drv_path.clone(), d.dependencies.clone()));
            } else {
                for dep in &missing {
                    self.waiters.entry(dep.clone()).or_default().insert(d.drv_path.clone());
                }
                self.deps.insert(d.drv_path.clone(), d.dependencies.clone());
                self.pending.insert(d.drv_path.clone(), missing);
            }
        }

        // 3. Each freshly seen drv_path may complete sources registered in earlier
        //    batches. No cascade: becoming ready never marks a new seen.
        for d in &fresh {
            let Some(srcs) = self.waiters.remove(&d.drv_path) else {
                continue;
            };

            for src in srcs {
                if let Some(missing) = self.pending.get_mut(&src) {
                    missing.remove(&d.drv_path);
                    if missing.is_empty() {
                        self.pending.remove(&src);
                        if let Some(deps) = self.deps.remove(&src) {
                            ready.push((src, deps));
                        }
                    }
                }
            }
        }

        ready
    }

    /// Drain sources whose deps never all materialised, so the end-of-eval flush
    /// can attempt their edges once the graph is final.
    pub(crate) fn drain_pending(&mut self) -> Vec<(String, Vec<String>)> {
        self.waiters.clear();
        self.pending.clear();
        self.deps.drain().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drv(path: &str, deps: &[&str]) -> DiscoveredDerivation {
        DiscoveredDerivation {
            attr: String::new(),
            drv_path: path.into(),
            outputs: vec![],
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
            architecture: "x86_64-linux".into(),
            required_features: vec![],
            timeout_secs: None,
            max_silent_secs: None,
            prefer_local_build: false,
            is_fixed_output: false,
            allow_substitutes: true,
            pname: None,
            substituted: false,
        }
    }

    fn paths(ready: &[(String, Vec<String>)]) -> Vec<String> {
        let mut v: Vec<String> = ready.iter().map(|(p, _)| p.clone()).collect();
        v.sort();
        v
    }

    #[test]
    fn leaf_ready_in_same_batch() {
        let mut t = EdgeReadiness::default();
        let r = t.observe(&[drv("leaf", &[])]);
        assert_eq!(paths(&r), vec!["leaf"]);
        assert!(r[0].1.is_empty(), "leaf carries no edges");
    }

    #[test]
    fn parent_waits_for_dep_in_later_batch() {
        let mut t = EdgeReadiness::default();
        assert!(
            t.observe(&[drv("parent", &["dep"])]).is_empty(),
            "parent not ready until dep has a row"
        );

        let r = t.observe(&[drv("dep", &[])]);
        assert_eq!(paths(&r), vec!["dep", "parent"]);
        let parent = r.iter().find(|(p, _)| p == "parent").unwrap();
        assert_eq!(parent.1, vec!["dep".to_string()], "parent edge recorded once");
    }

    #[test]
    fn parent_and_deps_same_batch_ready_together() {
        let mut t = EdgeReadiness::default();
        let r = t.observe(&[drv("parent", &["a", "b"]), drv("a", &[]), drv("b", &[])]);
        assert_eq!(paths(&r), vec!["a", "b", "parent"]);
    }

    #[test]
    fn multi_dep_ready_after_last_dep() {
        let mut t = EdgeReadiness::default();
        assert!(t.observe(&[drv("p", &["a", "b"])]).is_empty());
        assert_eq!(paths(&t.observe(&[drv("a", &[])])), vec!["a"], "p still waits on b");
        assert_eq!(paths(&t.observe(&[drv("b", &[])])), vec!["b", "p"]);
    }

    #[test]
    fn diamond_each_ready_once_with_full_edge_set() {
        let mut t = EdgeReadiness::default();
        let mut all: Vec<(String, Vec<String>)> = Vec::new();
        all.extend(t.observe(&[drv("a", &["b", "c"])]));
        all.extend(t.observe(&[drv("b", &["d"]), drv("c", &["d"])]));
        all.extend(t.observe(&[drv("d", &[])]));

        assert_eq!(paths(&all), vec!["a", "b", "c", "d"], "each node ready exactly once");

        let mut edges: Vec<(String, String)> = all
            .iter()
            .flat_map(|(src, deps)| deps.iter().map(move |dep| (src.clone(), dep.clone())))
            .collect();
        edges.sort();
        assert_eq!(
            edges,
            vec![
                ("a".into(), "b".into()),
                ("a".into(), "c".into()),
                ("b".into(), "d".into()),
                ("c".into(), "d".into()),
            ]
        );
    }

    #[test]
    fn reobserving_seen_drv_yields_nothing() {
        let mut t = EdgeReadiness::default();
        let _ = t.observe(&[drv("leaf", &[])]);
        assert!(
            t.observe(&[drv("leaf", &[])]).is_empty(),
            "already-seen leaf not re-promoted"
        );
    }

    #[test]
    fn drain_pending_returns_unresolved_sources() {
        let mut t = EdgeReadiness::default();
        let _ = t.observe(&[drv("p", &["never"])]);
        assert_eq!(
            t.drain_pending(),
            vec![("p".to_string(), vec!["never".to_string()])]
        );
    }
}
