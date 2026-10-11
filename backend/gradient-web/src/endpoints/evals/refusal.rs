/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::refusal::{Refusal, RefusalStatus};

#[derive(Debug, thiserror::Error)]
pub(crate) enum EvaluationRefusal {
    #[error("The evaluation is still running")]
    StillRunning,
    #[error("The evaluation belongs to no task")]
    WithoutTask,
    #[error("Another evaluation of the task is running")]
    TaskAlreadyEvaluating,
    #[error("Instance context requires the ManageWorkers permission")]
    ManageWorkersRequired,
}

impl Refusal for EvaluationRefusal {
    fn status(&self) -> RefusalStatus {
        match self {
            Self::StillRunning | Self::WithoutTask | Self::TaskAlreadyEvaluating => {
                RefusalStatus::Conflict
            }
            Self::ManageWorkersRequired => RefusalStatus::Forbidden,
        }
    }
}
