/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Shared logic for creating a queued evaluation from any trigger source
//! (API endpoint, incoming forge webhook, …) and for restarting the failed
//! builds of a previous evaluation.

mod flake_snapshot;
mod input_update;
mod new_evaluation;
mod restart;

use thiserror::Error;

pub use input_update::maybe_trigger_input_update;
pub use new_evaluation::trigger_evaluation;
pub use restart::trigger_restart_builds;

#[derive(Debug, Error)]
pub enum TriggerError {
    #[error("evaluation already in progress for this project")]
    AlreadyInProgress,
    #[error("no previous evaluation found to restart from")]
    NoPreviousEvaluation,
    #[error("database error: {0}")]
    Db(#[from] sea_orm::DbErr),
}

#[cfg(test)]
mod tests;
