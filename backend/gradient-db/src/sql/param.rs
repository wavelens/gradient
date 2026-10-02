/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Param {
    DerivationId,
    DerivationIds(usize),
    /// A real orphan candidate has no `build_job` and no `entry_point`.
    /// A plain `DerivationIds` draw would hit the restrict, and the delete would go unmeasured.
    OrphanDerivationIds(usize),
    DerivationHash,
    DerivationHashes(usize),
    CachedPathId,
    CachedPathHash,
    CachedPathHashes(usize),
    SharedBuildId,
    SharedBuildIds(usize),
    EvaluationId,
    EvaluationIds(usize),
    EntryPointId,
    EntryPointIds(usize),
    ProjectId,
    UserId,
    CacheId,
    CacheIds(usize),
    TaskId,
    TaskActionId,
    IntegrationId,
    CommitPrefixLow,
    CommitPrefixHigh,
    NewUuid,
    NewUuids(usize),
    Text(&'static str),
    Int(i64),
    Bool(bool),
    Texts(&'static str, usize),
    Ints(i64, usize),
    Bools(bool, usize),
    Now,
}

impl Param {
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
