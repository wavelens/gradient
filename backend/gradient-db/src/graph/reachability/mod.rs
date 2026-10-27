/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod adoption;
mod build_jobs;
mod outputs;

pub use adoption::{
    Adopted, adopt_below_named, adopt_pending_closure, adopt_pending_closures, adopt_unnamed_below,
    pending_orphan_frontier, pending_orphans_among,
};
pub use build_jobs::{
    build_jobs_for_derivation, build_jobs_for_derivations, derivation_is_reachable,
    derivations_by_id, eval_any_shared_build_failed, eval_blocked, evals_referencing_derivation,
    evals_referencing_derivations, shared_build_status, shared_builds_by_derivation,
};
pub use outputs::{
    derivations_with_hashes, inherit_names, private_output_hashes, producers_of_hashes,
};
