/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Per-job phase timeline: scoped spans measured from the moment the worker
//! accepted the job.

use std::sync::Arc;
use std::time::Instant;

use gradient_types::proto::{JobPhase, JobPhaseSpan};
use gradient_util::sync::Mutex;

/// Ceiling on the spans one job records. A large eval pushes a NAR per closure
/// member, so an uncapped timeline would put tens of thousands of spans in the
/// terminal message and a row each in the database. Past the cap the phase
/// still runs, it just stops being timed individually.
const MAX_SPANS: usize = 2_000;

/// Records how one job spent its time. Cloned into every phase guard, so all
/// mutation goes through the interior lock.
pub struct JobTimeline {
    start: Instant,
    spans: Mutex<Vec<JobPhaseSpan>>,
    /// Indices of the spans still open, innermost last.
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

    /// Open a span that closes when the returned guard drops. Past `MAX_SPANS`
    /// the guard is inert and the phase is counted as dropped instead.
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

    /// How many spans the cap discarded, for the log line on job completion.
    pub fn dropped(&self) -> u64 {
        *self.dropped.lock()
    }

    /// Every span recorded so far. Spans still open are reported closed at the
    /// current offset so a failed job still yields a usable timeline.
    pub fn snapshot(&self) -> Vec<JobPhaseSpan> {
        let now = self.elapsed_ms();
        let open = self.open.lock().clone();
        let mut spans = self.spans.lock().clone();
        for index in open {
            if let Some(span) = spans.get_mut(index as usize) {
                span.end_ms = now;
            }
        }

        spans
    }

    fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis() as u64
    }
}

/// Closes its span on drop. `record` attaches the path and byte counters the
/// board shows next to the duration.
pub struct PhaseGuard {
    timeline: Arc<JobTimeline>,
    /// `None` once the span cap is reached; the guard then does nothing.
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

        // Removed by identity rather than popped: concurrent phases inside one
        // job would otherwise close each other's spans.
        self.timeline.open.lock().retain(|i| *i != index);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A span opened inside another records the outer one as its parent, which
    /// is the whole point: the board draws NAR pushes underneath compress.
    #[test]
    fn an_inner_span_records_its_parent() {
        let t = JobTimeline::new();
        {
            let _outer = t.enter(JobPhase::Compress);
            let _inner = t.enter(JobPhase::NarPush);
        }

        let spans = t.snapshot();
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].phase, JobPhase::Compress);
        assert_eq!(spans[0].parent, None);
        assert_eq!(spans[1].phase, JobPhase::NarPush);
        assert_eq!(spans[1].parent, Some(0));
    }

    /// Siblings share a parent rather than chaining off each other.
    #[test]
    fn siblings_share_the_enclosing_parent() {
        let t = JobTimeline::new();
        let _outer = t.enter(JobPhase::Compress);
        drop(t.enter(JobPhase::NarPush));
        drop(t.enter(JobPhase::NarPush));

        let spans = t.snapshot();
        assert_eq!(spans[1].parent, Some(0));
        assert_eq!(spans[2].parent, Some(0));
    }

    /// A span left open when the job fails is still reported, closed at the
    /// moment of the snapshot, so a partial timeline reaches the board.
    #[test]
    fn an_open_span_is_closed_by_the_snapshot() {
        let t = JobTimeline::new();
        let _open = t.enter(JobPhase::Build);

        let spans = t.snapshot();
        assert_eq!(spans.len(), 1);
        assert!(spans[0].end_ms >= spans[0].start_ms);
    }

    /// Offsets run from job start, not from the enclosing span, and never move
    /// backwards.
    #[test]
    fn offsets_are_monotonic_from_job_start() {
        let t = JobTimeline::new();
        let first = {
            drop(t.enter(JobPhase::Fetch));
            t.snapshot()[0]
        };
        std::thread::sleep(std::time::Duration::from_millis(5));
        drop(t.enter(JobPhase::EvalFlake));

        let spans = t.snapshot();
        assert_eq!(spans[0].start_ms, first.start_ms);
        assert!(spans[1].start_ms >= spans[0].end_ms);
    }

    /// A job that opens more phases than the cap keeps the first `MAX_SPANS`
    /// and counts the rest, rather than growing the terminal message without
    /// bound. A large eval pushes one NAR per closure member, so this is the
    /// normal case, not a pathological one.
    #[test]
    fn the_span_cap_bounds_the_timeline() {
        let t = JobTimeline::new();
        for _ in 0..MAX_SPANS + 10 {
            drop(t.enter(JobPhase::NarPush));
        }

        assert_eq!(t.snapshot().len(), MAX_SPANS);
        assert_eq!(t.dropped(), 10);
    }

    /// An inert guard must not corrupt the nesting of the spans around it.
    #[test]
    fn a_capped_guard_leaves_nesting_intact() {
        let t = JobTimeline::new();
        let _outer = t.enter(JobPhase::Compress);
        for _ in 0..MAX_SPANS + 5 {
            drop(t.enter(JobPhase::NarPush));
        }
        drop(t.enter(JobPhase::NarPush));

        let spans = t.snapshot();
        assert_eq!(spans.len(), MAX_SPANS);
        assert!(
            spans[1..].iter().all(|s| s.parent == Some(0)),
            "a dropped span must not become a parent"
        );
    }

    /// Detail counters ride on the span so the board can show throughput.
    #[test]
    fn a_guard_records_paths_and_bytes() {
        let t = JobTimeline::new();
        {
            let mut g = t.enter(JobPhase::NarPush);
            g.record(7, 1024);
        }

        let spans = t.snapshot();
        assert_eq!(spans[0].paths, 7);
        assert_eq!(spans[0].bytes, 1024);
    }
}
