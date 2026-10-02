/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::time::Instant;

use gradient_util::sync::Mutex;
use gradient_wire::types::{JobPhase, JobPhaseSpan};

/// A large eval is pushing one NAR per closure member. An uncapped timeline would put tens of
/// thousands of spans into the terminal message and the database.
const MAX_SPANS: usize = 2_000;

pub struct JobTimeline {
    start: Instant,
    spans: Mutex<Vec<JobPhaseSpan>>,
    open: Mutex<Vec<u32>>,
    dropped: Mutex<u64>,
}

impl JobTimeline {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            spans: Mutex::new(Vec::new()),
            open: Mutex::new(Vec::new()),
            dropped: Mutex::new(0),
        })
    }

    pub fn enter(self: &Arc<Self>, phase: JobPhase) -> PhaseGuard {
        let start_ms = self.elapsed_ms();
        let parent = self.open.lock().last().copied();
        let index = {
            let mut spans = self.spans.lock();
            if spans.len() >= MAX_SPANS {
                *self.dropped.lock() += 1;
                None
            } else {
                spans.push(JobPhaseSpan {
                    phase,
                    start_ms,
                    end_ms: start_ms,
                    parent,
                    ..Default::default()
                });
                Some((spans.len() - 1) as u32)
            }
        };
        if let Some(index) = index {
            self.open.lock().push(index);
        }

        PhaseGuard {
            timeline: Arc::clone(self),
            index,
            paths: 0,
            bytes: 0,
        }
    }

    pub fn dropped(&self) -> u64 {
        *self.dropped.lock()
    }

    pub fn snapshot(&self) -> TimelineSnapshot {
        let elapsed_ms = self.elapsed_ms();
        let open = self.open.lock().clone();
        let mut spans = self.spans.lock().clone();
        for index in open {
            if let Some(span) = spans.get_mut(index as usize) {
                span.end_ms = elapsed_ms;
            }
        }

        TimelineSnapshot { spans, elapsed_ms }
    }

    fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

pub struct TimelineSnapshot {
    pub spans: Vec<JobPhaseSpan>,
    pub elapsed_ms: u64,
}

pub struct PhaseGuard {
    timeline: Arc<JobTimeline>,
    index: Option<u32>,
    paths: u32,
    bytes: u64,
}

impl PhaseGuard {
    pub fn record(&mut self, paths: u32, bytes: u64) {
        self.paths = self.paths.saturating_add(paths);
        self.bytes = self.bytes.saturating_add(bytes);
    }
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        let Some(index) = self.index else {
            return;
        };

        let end_ms = self.timeline.elapsed_ms();
        if let Some(span) = self.timeline.spans.lock().get_mut(index as usize) {
            span.end_ms = end_ms;
            span.paths = self.paths;
            span.bytes = self.bytes;
        }

        // Spans are removed by identity rather than popped. Concurrent phases inside one job would
        // otherwise close each other's spans.
        self.timeline.open.lock().retain(|i| *i != index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_inner_span_records_its_parent() {
        let t = JobTimeline::new();
        {
            let _outer = t.enter(JobPhase::Compress);
            let _inner = t.enter(JobPhase::NarPush);
        }

        let spans = t.snapshot().spans;
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].phase, JobPhase::Compress);
        assert_eq!(spans[0].parent, None);
        assert_eq!(spans[1].phase, JobPhase::NarPush);
        assert_eq!(spans[1].parent, Some(0));
    }

    #[test]
    fn siblings_share_the_enclosing_parent() {
        let t = JobTimeline::new();
        let _outer = t.enter(JobPhase::Compress);
        drop(t.enter(JobPhase::NarPush));
        drop(t.enter(JobPhase::NarPush));

        let spans = t.snapshot().spans;
        assert_eq!(spans[1].parent, Some(0));
        assert_eq!(spans[2].parent, Some(0));
    }

    #[test]
    fn an_open_span_is_closed_by_the_snapshot() {
        let t = JobTimeline::new();
        let _open = t.enter(JobPhase::Build);

        let spans = t.snapshot().spans;
        assert_eq!(spans.len(), 1);
        assert!(spans[0].end_ms >= spans[0].start_ms);
    }

    #[test]
    fn offsets_are_monotonic_from_job_start() {
        let t = JobTimeline::new();
        let first = {
            drop(t.enter(JobPhase::Fetch));
            t.snapshot().spans[0]
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
        drop(t.enter(JobPhase::EvalFlake));

        let spans = t.snapshot().spans;
        assert_eq!(spans[0].start_ms, first.start_ms);
        assert!(spans[1].start_ms >= spans[0].end_ms);
    }

    #[test]
    fn the_span_cap_bounds_the_timeline() {
        let t = JobTimeline::new();
        for _ in 0..MAX_SPANS + 10 {
            drop(t.enter(JobPhase::NarPush));
        }

        assert_eq!(t.snapshot().spans.len(), MAX_SPANS);
        assert_eq!(t.dropped(), 10);
    }

    #[test]
    fn the_snapshot_clock_covers_every_span() {
        let t = JobTimeline::new();
        drop(t.enter(JobPhase::Prefetch));
        let _open = t.enter(JobPhase::Build);
        std::thread::sleep(std::time::Duration::from_millis(2));

        let snapshot = t.snapshot();
        let last_end = snapshot.spans.iter().map(|s| s.end_ms).max().unwrap();
        assert_eq!(snapshot.elapsed_ms, last_end);
    }

    #[test]
    fn a_capped_guard_leaves_nesting_intact() {
        let t = JobTimeline::new();
        let _outer = t.enter(JobPhase::Compress);
        for _ in 0..MAX_SPANS + 5 {
            drop(t.enter(JobPhase::NarPush));
        }
        drop(t.enter(JobPhase::NarPush));

        let spans = t.snapshot().spans;
        assert_eq!(spans.len(), MAX_SPANS);
        assert!(
            spans[1..].iter().all(|s| s.parent == Some(0)),
            "a dropped span must not become a parent"
        );
    }
}
