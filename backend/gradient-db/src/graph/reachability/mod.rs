/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Evaluation <-> derivation reachability over the global graph. A `build_job`
//! row is how an evaluation names a derivation it waits on, and every reader of
//! "does some evaluation still want this" (the promotion gate, the dispatch
//! select, the dispatcher's driving evaluation, eval-done, the abort's shared
//! set) reads the row rather than walking the graph. A walk names what it
//! recorded plus the direct inputs of that, so the interior of a pruned subtree
//! is named by the evaluation that walked it and by nobody else; adoption is what
//! keeps the rows true when that evaluation is deleted, or a thaw, a reset or a
//! retire leaves an open shared build unnamed, while another evaluation still waits on
//! the subtree.

mod adoption;
mod build_jobs;
mod outputs;

pub use adoption::{
    Adopted, adopt_pending_closure, adopt_pending_closures, pending_orphan_frontier,
    pending_orphans_among,
};
pub use build_jobs::{
    build_jobs_for_derivation, build_jobs_for_derivations, derivation_is_reachable,
    eval_any_shared_build_failed, eval_blocked, evals_referencing_derivation,
    evals_referencing_derivations, shared_build_status,
};
pub use outputs::{
    derivations_with_hashes, inherit_names, private_output_hashes, producers_of_hashes,
};
