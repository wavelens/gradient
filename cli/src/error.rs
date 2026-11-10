/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::output::ExitKind;
use connector::ConnectorError;
use std::fmt::{self, Display};
use std::io;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileAction {
    Create,
    Read,
    Write,
    Stage,
}

impl Display for FileAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Create => "create",
            Self::Read => "read",
            Self::Write => "write",
            Self::Stage => "stage",
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum CliError {
    #[error(
        "No project selected. Pass `--project <name>` or run `gradient project select <name>` first."
    )]
    NoProjectSelected,
    #[error("No task selected. Pass `--task <name>` or run `gradient task select <name>` first.")]
    NoTaskSelected,
    #[error("Server URL not set. Run `gradient login <url>` to set the server and authenticate.")]
    ServerNotSet,
    #[error(
        "A saved account named '{0}' does not exist. Run `gradient account list` to list saved accounts or run `gradient --account {0} login <url>` to add one."
    )]
    AccountNotFound(String),
    #[error(
        "No default account is set. Run `gradient account switch <name>` to choose one, or pass `--account <name>`."
    )]
    NoDefaultAccountSelected,
    #[error("Not logged in, or the session expired. Run `gradient login <url>` first.")]
    NotLoggedIn,
    #[error("Authentication expired before approval.")]
    AuthorizationExpired,
    #[error("No argument: {0}.")]
    MissingArgument(&'static str),
    #[error("{0} cannot be empty.")]
    EmptyField(String),
    #[error("{0} needs to be an integer.")]
    NotAnInteger(&'static str),
    #[error("Terminal UI failed: {0}")]
    TerminalUi(io::Error),
    #[error("Failed to read {0}: {1}")]
    ReadInput(String, io::Error),
    #[error("Failed to {action} {}: {reason}", path.display())]
    File {
        action: FileAction,
        path: PathBuf,
        reason: io::Error,
    },
    #[error(transparent)]
    Api(#[from] ConnectorError),
}

impl CliError {
    pub fn file(action: FileAction, path: impl Into<PathBuf>, reason: io::Error) -> Self {
        Self::File {
            action,
            path: path.into(),
            reason,
        }
    }

    pub fn exit_kind(&self) -> ExitKind {
        match self {
            Self::NoProjectSelected
            | Self::NoTaskSelected
            | Self::ServerNotSet
            | Self::AccountNotFound(_)
            | Self::NoDefaultAccountSelected
            | Self::MissingArgument(_)
            | Self::EmptyField(_)
            | Self::NotAnInteger(_)
            | Self::ReadInput(..) => ExitKind::Usage,
            Self::NotLoggedIn => ExitKind::Unauthorized,
            Self::AuthorizationExpired | Self::TerminalUi(_) | Self::File { .. } => ExitKind::Api,
            Self::Api(e) => e.into(),
        }
    }
}

impl From<&ConnectorError> for ExitKind {
    fn from(error: &ConnectorError) -> Self {
        match error {
            ConnectorError::Unauthorized => Self::Unauthorized,
            ConnectorError::InvalidTaskEvaluationOrBuildArgument(_) => Self::Usage,
            _ => Self::Api,
        }
    }
}
