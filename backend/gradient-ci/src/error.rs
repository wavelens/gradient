/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::ids::TaskId;
use gradient_util::http_validation::DestinationUrlRejectedError;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ActionError {
    #[error("task {id} not found")]
    TaskNotFound { id: TaskId },
    #[error("invalid evaluation_id")]
    InvalidEvaluationId,
    #[error("could not parse owner/repo from {repository}")]
    RepositoryUnparsable { repository: String },
    #[error("The pull request branch {branch} does not match the rules for git branch names")]
    InvalidPullRequestBranchName { branch: String },
    #[error("URL rejected: {cause}")]
    UrlRejected { cause: DestinationUrlRejectedError },
    #[error("loading crypt key: {cause}")]
    CryptKeyUnreadable { cause: anyhow::Error },
}
