/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::EvaluationId;
use thiserror::Error;

#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub enum MissingRow {
    #[error("evaluation {id} not found")]
    Evaluation { id: EvaluationId },
    #[error("unique violation but no active row found")]
    ActiveAdminTask,
}
