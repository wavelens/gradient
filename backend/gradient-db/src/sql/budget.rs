/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! What a query is allowed to cost. Every limit is a property of the plan, never
//! of the clock: the VM the gate runs in is shared and slow, so a millisecond
//! budget would only measure how busy the runner was.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Spill {
    Forbidden,
    Warn,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Shape {
    Require(&'static str),
    Forbid(&'static str),
}

#[derive(Copy, Clone, Debug)]
pub struct Budget {
    pub buffers: u64,
    pub amplification: u64,
    pub rows_removed: u64,
    pub loops: u64,
    /// Relation size above which a sequential scan is a failure. `None` permits
    /// any sequential scan, which is what a timer-driven sweep legitimately does.
    pub seq_scan_rows: Option<u64>,
    pub spill: Spill,
    pub shape: &'static [Shape],
    pub reason: Option<&'static str>,
}

impl Budget {
    pub const HOT: Self = Self {
        buffers: 2_000,
        amplification: 100,
        rows_removed: 10_000,
        loops: 1_000,
        seq_scan_rows: Some(10_000),
        spill: Spill::Forbidden,
        shape: &[],
        reason: None,
    };

    /// A bulk statement reads the working set it was handed or must rank, so a
    /// sequential scan of one table is its normal plan; what it may not do is
    /// read the database.
    pub const BULK: Self = Self {
        buffers: 50_000,
        amplification: 10_000,
        rows_removed: 1_000_000,
        loops: u64::MAX,
        seq_scan_rows: Some(10_000),
        spill: Spill::Warn,
        shape: &[],
        reason: None,
    };

    pub const WALK: Self = Self {
        buffers: 250_000,
        amplification: 1_000,
        rows_removed: 1_000_000,
        loops: u64::MAX,
        seq_scan_rows: Some(10_000),
        spill: Spill::Forbidden,
        shape: &[Shape::Require("Nested Loop")],
        reason: None,
    };

    pub const SWEEP: Self = Self {
        buffers: 1_000_000,
        amplification: 10_000,
        rows_removed: u64::MAX,
        loops: u64::MAX,
        seq_scan_rows: None,
        spill: Spill::Warn,
        shape: &[],
        reason: None,
    };

    pub const fn hot() -> Self {
        Self::HOT
    }

    pub const fn bulk() -> Self {
        Self::BULK
    }

    pub const fn walk() -> Self {
        Self::WALK
    }

    pub const fn sweep() -> Self {
        Self::SWEEP
    }

    pub const fn buffers(mut self, n: u64) -> Self {
        self.buffers = n;
        self
    }

    pub const fn amplification(mut self, n: u64) -> Self {
        self.amplification = n;
        self
    }

    pub const fn rows_removed(mut self, n: u64) -> Self {
        self.rows_removed = n;
        self
    }

    pub const fn loops(mut self, n: u64) -> Self {
        self.loops = n;
        self
    }

    pub const fn seq_scan_allowed(mut self) -> Self {
        self.seq_scan_rows = None;
        self
    }

    /// Drops the fence assertion, for a recursion that joins a materialised set
    /// rather than a fenced `LATERAL`.
    pub const fn unfenced(mut self) -> Self {
        self.shape = &[];
        self
    }

    /// Records why a query is over the tier's ceiling. An override without one is
    /// an exemption nobody can audit, and the gate warns when the query it
    /// covers stops needing it.
    pub const fn because(mut self, reason: &'static str) -> Self {
        self.reason = Some(reason);
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Violation {
    pub rule: &'static str,
    pub detail: String,
    pub fatal: bool,
}
