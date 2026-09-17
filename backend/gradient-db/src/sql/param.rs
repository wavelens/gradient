/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The kinds a declared parameter can take. The gate draws a real value of the
//! kind from the live dataset, so no query carries a literal fixture that goes
//! stale when the data behind it changes.

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Param {
    DerivationId,
    DerivationIds(usize),
    DerivationHash,
    DerivationHashes(usize),
    CachedPathId,
    CachedPathHash,
    CachedPathHashes(usize),
    /// A `derivation_build` id: the global build-once anchor.
    AnchorId,
    AnchorIds(usize),
    EvaluationId,
    EvaluationIds(usize),
    EntryPointId,
    EntryPointIds(usize),
    ProjectId,
    OrganizationId,
    UserId,
    CacheId,
    TaskId,
    Text(&'static str),
    Int(i64),
    Bool(bool),
    Now,
}

impl Param {
    /// True for the kinds the gate has to read out of the database, false for
    /// the ones it can spell out on its own.
    pub const fn is_drawn(self) -> bool {
        !matches!(
            self,
            Self::Text(_) | Self::Int(_) | Self::Bool(_) | Self::Now
        )
    }
}
