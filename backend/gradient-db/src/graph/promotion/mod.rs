/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared build transitions that are not the can-start state promotion: the substitution of
//! shared builds an evaluation found complete in our cache, the failure cascade and its
//! eval-scoped sweep, the requeue thaws, and the dispatch gate. Promotion itself
//! lives in [`crate::graph::can_start`], which owns the `fetchable` / `blocking_deps`
//! counters every gate is built from.
//!
//! The dispatch gate reads the queue invariant instead of re-deriving can-start state:
//! `Queued` means [`crate::graph::predicates::gates_predicate`] held when the shared build was
//! promoted, and the event that breaks one of those gates un-promotes the row.
//! Re-evaluating the gates per dispatch candidate is the per-row work #591 removed.

mod cached;
mod failure;
mod requeue;
mod startable;
mod transitions;

pub use cached::{repair_cached_shared_builds_for_eval, substitute_created_shared_builds};
pub use failure::{cascade_dependency_failed, repair_dependency_failed};
pub use requeue::{requeue_failed_closure, requeue_failed_shared_builds};
pub use startable::{find_startable_shared_builds, find_startable_shared_builds_among};
pub(crate) use transitions::{returned_derivations, returned_transitions, transitions_from};
