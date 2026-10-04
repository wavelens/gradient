/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! `blocking_deps` is moved and not derived.
//! Every ripple must be driven by a transition, never by a state.
//! A parent moved past zero is never satisfying `= 0` again.
//!
//! `blocking_deps` must not get a `CHECK (blocking_deps >= 0)` constraint.
//! A ripple is passing through intermediate values inside its own transaction.

mod fetchable;
mod lock;
mod need;
mod queue;
mod repair;
#[cfg(test)]
mod test_rows;

pub use fetchable::{advance_fetchable, became_fetchable, lost_fetchability, seed_blocking_deps};
pub(crate) use lock::ids;
pub use lock::{SeedLock, SharedBuildLock, lock_seed_shared_builds, lock_shared_builds};
pub use need::{NeedMoved, SettledNeed, recount_wanted, settle_skipped, update_and_settle_need};
pub(crate) use need::{settle_need, update_need};
pub use queue::{
    promote, promote_closure, unpromote_drv_owners, unpromote_ungated, unwalk_derivations,
};
pub use repair::{
    Repaired, RepairedFetchable, can_start_scope, repair_can_start, repair_fetchable,
};
