/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared evaluation and build status helpers: state-machine-guarded status
//! transitions ([`derivation_build_status`], [`evaluation_status`]), evaluation
//! abort ([`abort`]), and the best-effort phase/message logging ([`logging`]).

mod abort;
mod derivation_build_status;
mod effects;
mod eval_finalize;
mod evaluation_status;
mod logging;

pub use abort::abort_evaluation;
pub use derivation_build_status::{
    announce_entry_point_statuses, notify_build_status_for_derivations,
    update_derivation_build_status,
};
pub use effects::{TransitionChange, emit_transition_effects};
pub use eval_finalize::{check_evaluation_done, finalize_evals_for_derivations};
pub use evaluation_status::{update_evaluation_status, update_evaluation_status_with_error};
pub use logging::{
    PhaseSubjectKind, finalize_build_log, insert_evaluation_message, record_evaluation_message,
    record_phase_event,
};
