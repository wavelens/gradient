// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use std::fmt;

#[non_exhaustive]
#[derive(Debug)]
pub enum ConnectorError {
    Api {
        status: reqwest::StatusCode,
        message: String,
    },
    Unauthorized,
    WebBrowserSignInRequired,
    Transport(reqwest::Error),
    Decode(serde_json::Error),
    Io(std::io::Error),
    InvalidTaskEvaluationOrBuildArgument(String),
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api { status, message } => {
                write!(f, "api error ({}): {}", status.as_u16(), message)
            }
            Self::Unauthorized => f.write_str("unauthorized: token missing or rejected"),
            Self::WebBrowserSignInRequired => f.write_str(
                "This operation needs a session from sign-in on the web interface. The browser flow of `gradient login` can't sign in for this operation. API keys don't work either. Run the operation on the web interface, or sign in with `gradient login --username <USERNAME>` if the account has a password.",
            ),
            Self::Transport(e) => write!(f, "transport error: {}", e),
            Self::Decode(e) => write!(f, "decode error: {}", e),
            Self::Io(e) => write!(f, "io error: {}", e),
            Self::InvalidTaskEvaluationOrBuildArgument(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for ConnectorError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(e) => Some(e),
            Self::Decode(e) => Some(e),
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<reqwest::Error> for ConnectorError {
    fn from(e: reqwest::Error) -> Self {
        Self::Transport(e)
    }
}

impl From<serde_json::Error> for ConnectorError {
    fn from(e: serde_json::Error) -> Self {
        Self::Decode(e)
    }
}

impl From<std::io::Error> for ConnectorError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
