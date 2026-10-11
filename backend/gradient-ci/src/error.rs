/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::TaskId;
use gradient_util::http_validation::WebhookUrlError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("task {id} not found")]
    TaskNotFound { id: TaskId },
    #[error("invalid evaluation_id")]
    InvalidEvaluationId,
    #[error("could not parse owner/repo from {repository}")]
    RepositoryUnparsable { repository: String },
    #[error("URL rejected: {cause}")]
    UrlRejected { cause: WebhookUrlError },
    #[error("loading crypt key: {cause}")]
    CryptKeyUnreadable { cause: anyhow::Error },
}
