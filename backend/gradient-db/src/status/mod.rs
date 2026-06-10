/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared evaluation and build status helpers: state-machine-guarded status
//! transitions ([`build_status`], [`evaluation_status`]), evaluation abort
//! ([`abort`]), cross-evaluation build-leader election ([`leader_election`]),
//! and the best-effort phase/message logging ([`logging`]).

mod abort;
mod build_status;
mod evaluation_status;
mod leader_election;
mod logging;

pub use abort::abort_evaluation;
pub use build_status::update_build_status;
pub use evaluation_status::{update_evaluation_status, update_evaluation_status_with_error};
pub use leader_election::find_active_leaders;
pub use logging::{
    PHASE_SUBJECT_BUILD, PHASE_SUBJECT_EVALUATION, finalize_build_log, insert_evaluation_message,
    record_evaluation_message, record_phase_event,
};

#[cfg(test)]
mod tests;
