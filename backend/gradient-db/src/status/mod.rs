/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod abort;
mod derivation_build_status;
mod effects;
mod eval_finalize;
mod evaluation_build;
mod evaluation_message;
mod evaluation_status;
pub mod log_tail;
pub mod logging;

pub use abort::{abort_eval_shared_builds, release_evaluation_need};
pub use derivation_build_status::{
    announce_entry_point_statuses, build_change, notify_build_status_for_derivations,
    update_derivation_build_status,
};
pub use effects::{TransitionChange, collapse_transitions, emit_transition_effects};
pub use eval_finalize::{check_evaluation_done, finalize_evals_for_derivations};
pub use evaluation_build::{
    BuildRefusal, abort_build_for_all_evaluations, abort_evaluation_build, retry_evaluation_build,
    wanted_again,
};
pub use evaluation_message::{EvaluationFailure, EvaluationWarning};
pub use evaluation_status::{
    change_evaluation_status, evaluation_change, update_evaluation_status,
    update_evaluation_status_with_error,
};
pub use logging::{
    PhaseSubjectKind, enqueue_log_finalize, finalize_build_log, insert_worker_message,
    record_phase_event, record_phase_events, report_worker_message,
};
