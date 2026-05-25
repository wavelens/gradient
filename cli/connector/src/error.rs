use std::fmt;

#[non_exhaustive]
#[derive(Debug)]
pub enum ConnectorError {
    Api {
        status: reqwest::StatusCode,
        message: String,
    },
    Unauthorized,
    Transport(reqwest::Error),
    Decode(serde_json::Error),
    Io(std::io::Error),
}

impl fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Api { status, message } => {
                write!(f, "api error ({}): {}", status.as_u16(), message)
            }
            Self::Unauthorized => f.write_str("unauthorized: token missing or rejected"),
            Self::Transport(e) => write!(f, "transport error: {}", e),
            Self::Decode(e) => write!(f, "decode error: {}", e),
            Self::Io(e) => write!(f, "io error: {}", e),
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
