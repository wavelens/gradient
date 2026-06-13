/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! In-subprocess warm-fork pool: a coordinator that dispatches attr resolution
//! to a pool of warm fork children, monitors their RSS, and re-forks on crash
//! or RSS-threshold. The `Child` trait abstracts the OS fork so the policy is
//! unit-testable with a stub.

use crate::nix::eval_worker::ResolvedItem;

/// How many times an attr may crash a child before it is recorded as a
/// per-attr error instead of being retried.
const MAX_CRASH_ATTEMPTS: u32 = 2;

/// One attr handed to a child plus its 0-based index in the original request.
#[derive(Clone, Debug)]
pub struct WorkItem {
    pub index: usize,
    pub attr: String,
}

/// What a child reports back for one attr.
#[derive(Debug)]
pub enum ChildEvent {
    /// Resolved (success or per-attr eval error captured by the child).
    Done(ResolvedItem),
    /// The child process died before producing a result for its current attr.
    Crashed,
}

/// Abstraction over one warm fork child. Real impl forks; tests stub it.
pub trait Child {
    /// Hand the child one attr to resolve. An `Err` aborts the whole batch; a
    /// child that has died should instead surface via `recv` → `Crashed`.
    fn dispatch(&mut self, item: &WorkItem) -> std::io::Result<()>;
    /// Block until the child reports the dispatched attr's result or dies.
    fn recv(&mut self) -> ChildEvent;
    /// Current resident set size in bytes (0 if unknown).
    fn rss_bytes(&self) -> u64;
    /// Kill the child and reap it.
    fn kill(&mut self);
}

/// Spawns fresh warm children. Real impl re-forks from the warm parent; tests
/// return stubs.
pub trait ChildFactory {
    type C: Child;
    fn spawn(&mut self) -> std::io::Result<Self::C>;
}

/// Drive `attrs` through warm children. Each child resolves one attr at a time;
/// a child whose RSS exceeds `max_rss` after a result is killed and re-forked; a
/// child that crashes mid-attr is re-forked and the attr retried once, then
/// recorded as a per-attr error. Returns one `ResolvedItem` per input attr, in
/// input order. `pool` bounds how many children exist at once.
pub fn run_queue<F: ChildFactory>(
    factory: &mut F,
    attrs: Vec<String>,
    pool: usize,
    max_rss: u64,
) -> std::io::Result<Vec<ResolvedItem>> {
    // `pool` bounds concurrent children in the real fork impl; this policy is
    // serial, so it is unused for now.
    let _ = pool;
    let mut results: Vec<Option<ResolvedItem>> = (0..attrs.len()).map(|_| None).collect();
    let mut queue: std::collections::VecDeque<WorkItem> = attrs
        .into_iter()
        .enumerate()
        .map(|(index, attr)| WorkItem { index, attr })
        .collect();
    let mut attempts: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();

    let mut child = factory.spawn()?;
    while let Some(item) = queue.pop_front() {
        child.dispatch(&item)?;
        match child.recv() {
            ChildEvent::Done(r) => {
                results[item.index] = Some(r);
                if child.rss_bytes() > max_rss {
                    child.kill();
                    child = factory.spawn()?;
                }
            }
            ChildEvent::Crashed => {
                child.kill();
                child = factory.spawn()?;
                let n = attempts.entry(item.index).or_insert(0);
                *n += 1;
                if *n >= MAX_CRASH_ATTEMPTS {
                    results[item.index] = Some(ResolvedItem {
                        attr: item.attr,
                        drv_path: None,
                        references: vec![],
                        error: Some(
                            "evaluator crashed while resolving this attribute".to_string(),
                        ),
                    });
                } else {
                    queue.push_front(item);
                }
            }
        }
    }
    Ok(results.into_iter().map(|r| r.expect("every attr resolved")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Scripted stub: each dispatched attr yields the next queued event.
    struct StubChild {
        rss: u64,
        events: VecDeque<ChildEvent>,
        pending: Option<WorkItem>,
    }
    impl Child for StubChild {
        fn dispatch(&mut self, item: &WorkItem) -> std::io::Result<()> {
            self.pending = Some(item.clone());
            Ok(())
        }
        fn recv(&mut self) -> ChildEvent {
            let item = self.pending.take().expect("recv without dispatch");
            match self.events.pop_front().unwrap_or(ChildEvent::Crashed) {
                ChildEvent::Done(mut r) => {
                    r.attr = item.attr;
                    ChildEvent::Done(r)
                }
                e => e,
            }
        }
        fn rss_bytes(&self) -> u64 {
            self.rss
        }
        fn kill(&mut self) {}
    }
    fn ok(attr: &str) -> ChildEvent {
        ChildEvent::Done(ResolvedItem {
            attr: attr.into(),
            drv_path: Some(format!("h-{attr}.drv")),
            references: vec![],
            error: None,
        })
    }
    struct StubFactory {
        scripts: VecDeque<VecDeque<ChildEvent>>,
        rss: u64,
    }
    impl ChildFactory for StubFactory {
        type C = StubChild;
        fn spawn(&mut self) -> std::io::Result<StubChild> {
            Ok(StubChild {
                rss: self.rss,
                events: self.scripts.pop_front().unwrap_or_default(),
                pending: None,
            })
        }
    }

    #[test]
    fn drains_queue_in_order() {
        let attrs = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let mut factory = StubFactory {
            scripts: VecDeque::from(vec![VecDeque::from(vec![ok("x"), ok("x"), ok("x")])]),
            rss: 10,
        };
        let out = run_queue(&mut factory, attrs, 1, u64::MAX).unwrap();
        assert_eq!(
            out.iter().map(|i| i.attr.clone()).collect::<Vec<_>>(),
            vec!["a", "b", "c"]
        );
        assert!(out.iter().all(|i| i.drv_path.is_some()));
    }

    #[test]
    fn crash_then_retry_then_error_isolates_one_attr() {
        let attrs = vec!["a".to_string(), "b".to_string()];
        // run_queue re-forks a FRESH child after every crash, so each crash
        // consumes one scripted child: c1 crashes on "a", c2 crashes on the
        // retry (→ "a" errored), c3 resolves "b".
        let mut factory = StubFactory {
            scripts: VecDeque::from(vec![
                VecDeque::from(vec![ChildEvent::Crashed]),
                VecDeque::from(vec![ChildEvent::Crashed]),
                VecDeque::from(vec![ok("x")]),
            ]),
            rss: 10,
        };
        let out = run_queue(&mut factory, attrs, 1, u64::MAX).unwrap();
        let a = out.iter().find(|i| i.attr == "a").unwrap();
        assert!(a.drv_path.is_none() && a.error.is_some(), "twice-crashed attr must be an error");
        let b = out.iter().find(|i| i.attr == "b").unwrap();
        assert!(b.drv_path.is_some(), "other attrs still resolve after a crash");
    }

    #[test]
    fn rss_over_threshold_recycles_child() {
        let attrs = vec!["a".to_string(), "b".to_string()];
        let mut factory = StubFactory {
            scripts: VecDeque::from(vec![
                VecDeque::from(vec![ok("x")]),
                VecDeque::from(vec![ok("x")]),
            ]),
            rss: 999,
        };
        let out = run_queue(&mut factory, attrs, 1, 100).unwrap();
        assert_eq!(out.len(), 2);
        assert!(out.iter().all(|i| i.drv_path.is_some()));
    }
}
