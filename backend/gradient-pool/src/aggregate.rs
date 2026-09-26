/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;

use gradient_wire::types::GradientCapabilities;

use crate::WorkerShared;

/// The pool seen from upstream as one worker.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Aggregate {
    pub capabilities: GradientCapabilities,
    pub architectures: BTreeSet<String>,
    pub system_features: BTreeSet<String>,
    pub slots: u32,
    pub cpu_count: u32,
    pub ram_total_mb: u64,
    pub cpu_core_score: u32,
    pub workers: usize,
}

pub fn aggregate<'a>(workers: impl IntoIterator<Item = &'a WorkerShared>) -> Aggregate {
    workers
        .into_iter()
        .fold(Aggregate::default(), |mut agg, w| {
            agg.capabilities |= w.capabilities.clone();
            agg.architectures.extend(w.architectures.iter().cloned());
            agg.system_features
                .extend(w.system_features.iter().cloned());
            agg.slots = agg.slots.saturating_add(w.max_concurrent_builds);
            agg.cpu_count = agg.cpu_count.saturating_add(w.cpu_count);
            agg.ram_total_mb = agg.ram_total_mb.saturating_add(w.ram_total_mb);
            agg.cpu_core_score = agg.cpu_core_score.max(w.cpu_core_score);
            agg.workers += 1;
            agg
        })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::sync::Arc;

    use gradient_wire::types::GradientCapabilities;
    use tokio::sync::mpsc;

    use crate::WorkerPool;
    use crate::session_port::SessionPort;

    fn port() -> Arc<dyn SessionPort> {
        let (tx, _rx) = mpsc::unbounded_channel();
        Arc::new(tx)
    }

    fn register(
        pool: &mut WorkerPool,
        id: &str,
        caps: GradientCapabilities,
        arch: &[&str],
        features: &[&str],
        slots: u32,
    ) {
        pool.register(id.into(), caps, HashSet::new(), port());
        pool.update_capabilities(
            id,
            arch.iter().map(|s| s.to_string()).collect(),
            features.iter().map(|s| s.to_string()).collect(),
            slots,
            8,
            16_000,
            1_000,
        );
    }

    fn build() -> GradientCapabilities {
        GradientCapabilities {
            build: true,
            ..Default::default()
        }
    }

    #[test]
    fn an_empty_pool_advertises_nothing() {
        let agg = WorkerPool::new().aggregate();
        assert_eq!(agg.workers, 0);
        assert_eq!(agg.slots, 0);
        assert!(!agg.capabilities.build && !agg.capabilities.eval);
        assert!(agg.architectures.is_empty());
    }

    #[test]
    fn flags_or_sets_union_and_capacity_sums() {
        let mut pool = WorkerPool::new();
        let eval = GradientCapabilities {
            eval: true,
            fetch: true,
            ..Default::default()
        };
        register(&mut pool, "a", build(), &["x86_64-linux"], &["kvm"], 4);
        register(
            &mut pool,
            "b",
            eval,
            &["aarch64-linux"],
            &["big-parallel"],
            2,
        );

        let agg = pool.aggregate();
        assert!(agg.capabilities.build && agg.capabilities.eval && agg.capabilities.fetch);
        assert!(!agg.capabilities.cache);
        assert_eq!(
            agg.architectures
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["aarch64-linux", "x86_64-linux"]
        );
        assert_eq!(
            agg.system_features
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["big-parallel", "kvm"]
        );
        assert_eq!(
            (
                agg.slots,
                agg.cpu_count,
                agg.ram_total_mb,
                agg.cpu_core_score
            ),
            (6, 16, 32_000, 1_000)
        );
        assert_eq!(agg.workers, 2);
    }

    #[test]
    fn a_draining_worker_does_not_count() {
        let mut pool = WorkerPool::new();
        register(&mut pool, "a", build(), &["x86_64-linux"], &[], 4);
        register(&mut pool, "b", build(), &["aarch64-linux"], &[], 2);
        pool.mark_draining("b");

        let agg = pool.aggregate();
        assert_eq!(agg.slots, 4);
        assert_eq!(agg.architectures.len(), 1);
    }
}
