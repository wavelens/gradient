/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod adoption;
mod build_jobs;
mod outputs;

pub use adoption::{
    Adopted, BUILD_SCAN_BATCH_SIZE, OpenBuildWindowScanResult, adopt_below_named,
    adopt_pending_closure, adopt_pending_closures, adopt_unnamed_below,
    adopt_unnamed_below_for_live_evaluations, pending_orphans_among,
    read_open_build_window_without_build_job,
};
pub use build_jobs::{
    build_jobs_for_derivation, build_jobs_for_derivations, derivation_is_reachable,
    derivations_by_id, eval_blocked, evals_referencing_derivation, evals_referencing_derivations,
    read_shared_build_failure_and_abort_flags, shared_build_status, shared_builds_by_derivation,
};
pub use outputs::{
    derivations_with_hashes, inherit_names, private_output_hashes, producers_of_hashes,
    producers_of_hashes_with_output_not_in_shared_build_unneeded_outputs,
};
