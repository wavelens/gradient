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
pub use lock::{SeedLock, SharedBuildLock, lock_seed_shared_builds, lock_shared_builds};
pub(crate) use lock::{ids, lock_unavailable};
pub use need::{
    BudgetedNeedUpdate, NeedMoved, SettledNeed, gain_and_settle_need, recheck_forgotten_need,
    recount_wanted, settle_created, settle_skipped, update_and_settle_need,
    update_and_settle_need_with_advisory_key_limit,
};
pub(crate) use need::{settle_need, update_need, wanted_by_its_evaluation};
pub use queue::{promote, promote_closure, unpromote_ungated, unwalk_derivations};
pub use repair::{
    RepairedFetchable, SWEEP_CHUNKS_PER_TRANSACTION, SWEEP_LOCK_CHUNK,
    UnpromotedAndPromotedStatusTransitions, can_start_scope,
    recount_blocking_deps_for_derivation_ids, repair_all_shared_build_start_gate_states,
    repair_fetchable,
};
