/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Mutex, MutexGuard};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize)]
pub struct JournalEntry {
    pub seq: u64,
    pub at_us: u64,
    pub duration_us: u64,
    pub conn: u64,
    pub op: &'static str,
    pub paths: Vec<String>,
    pub ok: bool,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Violation {
    BuildWithMissingInput { drv: String, missing: Vec<String> },
    ReferenceNotValid { path: String, reference: String },
    ContentAddressMismatch { claimed: String, computed: String },
    NarHashMismatch { path: String },
    RebuildOfValidOutput { drv: String, output: String },
    UnknownDerivation { drv: String },
    Unimplemented { op: String },
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OpStats {
    pub count: u64,
    pub failed: u64,
    pub p50_us: u64,
    pub p95_us: u64,
    pub max_us: u64,
}

#[derive(Default)]
struct Inner {
    next_seq: u64,
    entries: Vec<JournalEntry>,
    violations: Vec<Violation>,
}

#[derive(Default)]
pub struct Journal {
    inner: Mutex<Inner>,
}

pub struct OpTimer<'a> {
    journal: &'a Journal,
    conn: u64,
    op: &'static str,
    paths: Vec<String>,
    at_us: u64,
    started: Instant,
}

fn now_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_micros() as u64)
        .unwrap_or_default()
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn start(&self, conn: u64, op: &'static str, paths: Vec<String>) -> OpTimer<'_> {
        OpTimer {
            journal: self,
            conn,
            op,
            paths,
            at_us: now_us(),
            started: Instant::now(),
        }
    }

    pub fn violation(&self, violation: Violation) {
        tracing::warn!(?violation, "invariant violated");
        self.lock().violations.push(violation);
    }

    pub fn since(&self, seq: u64) -> Vec<JournalEntry> {
        self.lock()
            .entries
            .iter()
            .filter(|e| e.seq > seq)
            .cloned()
            .collect()
    }

    pub fn violations(&self) -> Vec<Violation> {
        self.lock().violations.clone()
    }

    pub fn reset(&self) {
        let mut inner = self.lock();
        inner.entries.clear();
        inner.violations.clear();
    }

    pub fn stats(&self) -> BTreeMap<String, OpStats> {
        let inner = self.lock();
        let mut durations: BTreeMap<&str, (Vec<u64>, u64)> = BTreeMap::new();
        for entry in &inner.entries {
            let slot = durations.entry(entry.op).or_default();
            slot.0.push(entry.duration_us);
            slot.1 += u64::from(!entry.ok);
        }

        durations
            .into_iter()
            .map(|(op, (mut ds, failed))| {
                ds.sort_unstable();
                let pick = |q: f64| ds[((ds.len() - 1) as f64 * q).round() as usize];
                let stats = OpStats {
                    count: ds.len() as u64,
                    failed,
                    p50_us: pick(0.5),
                    p95_us: pick(0.95),
                    max_us: ds.last().copied().unwrap_or_default(),
                };
                (op.to_owned(), stats)
            })
            .collect()
    }
}

impl OpTimer<'_> {
    pub fn finish(self, ok: bool, detail: Option<String>) -> u64 {
        let mut inner = self.journal.lock();
        inner.next_seq += 1;
        let seq = inner.next_seq;
        inner.entries.push(JournalEntry {
            seq,
            at_us: self.at_us,
            duration_us: self.started.elapsed().as_micros() as u64,
            conn: self.conn,
            op: self.op,
            paths: self.paths,
            ok,
            detail,
        });
        seq
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entries_are_sequenced_and_filtered_by_since() {
        let journal = Journal::new();
        let first = journal
            .start(1, "is_valid_path", vec!["/nix/store/a".into()])
            .finish(true, None);
        let second = journal
            .start(2, "query_path_info", vec![])
            .finish(false, Some("boom".into()));
        assert_eq!((first, second), (1, 2));
        let tail = journal.since(1);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].op, "query_path_info");
        assert_eq!(tail[0].detail.as_deref(), Some("boom"));
    }

    #[test]
    fn stats_group_by_op() {
        let journal = Journal::new();
        for _ in 0..3 {
            journal
                .start(1, "build_derivation", vec![])
                .finish(true, None);
        }

        let stats = journal.stats();
        assert_eq!(stats["build_derivation"].count, 3);
    }

    #[test]
    fn reset_clears_entries_and_violations_but_keeps_counting() {
        let journal = Journal::new();
        journal.start(1, "x", vec![]).finish(true, None);
        journal.violation(Violation::Unimplemented { op: "Foo".into() });
        journal.reset();
        assert!(journal.since(0).is_empty());
        assert!(journal.violations().is_empty());
        assert_eq!(journal.start(1, "y", vec![]).finish(true, None), 2);
    }

    #[test]
    fn violation_serializes_with_kind_tag() {
        let json = serde_json::to_value(Violation::UnknownDerivation { drv: "d".into() })
            .expect("serialize");
        assert_eq!(json["kind"], "unknown_derivation");
    }
}
