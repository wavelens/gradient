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
    /// Derivations the orphan collector could actually be handed: no `build_job`
    /// and no `entry_point` names them. Those two are the only references to
    /// `derivation` that do not cascade, and they are also the keep-set's seeds,
    /// so a real candidate has neither. A plain `DerivationIds` draw hits the
    /// restrict and the delete comes back unmeasured instead of measured.
    OrphanDerivationIds(usize),
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
    UserId,
    CacheId,
    TaskId,
    TaskActionId,
    IntegrationId,
    /// An id the STATEMENT mints rather than one the data already has: what an
    /// INSERT writes into its own primary key, which a drawn id would collide
    /// with.
    NewUuid,
    NewUuids(usize),
    Text(&'static str),
    Int(i64),
    Bool(bool),
    /// A literal array of that value, repeated to the declared width: what an
    /// `unnest($n::text[])` position takes, which the scalar kinds cannot bind.
    Texts(&'static str, usize),
    Ints(i64, usize),
    Bools(bool, usize),
    Now,
}

impl Param {
    /// True for the kinds the gate has to read out of the database, false for
    /// the ones it can spell out on its own.
    pub const fn is_drawn(self) -> bool {
        !matches!(
            self,
            Self::Text(_)
                | Self::Int(_)
                | Self::Bool(_)
                | Self::NewUuid
                | Self::NewUuids(_)
                | Self::Texts(..)
                | Self::Ints(..)
                | Self::Bools(..)
                | Self::Now
        )
    }
}
