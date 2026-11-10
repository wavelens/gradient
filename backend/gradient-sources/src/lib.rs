/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod build_log;
pub mod cache_key;
pub mod daemon;
pub mod error;
pub mod evaluator;
pub mod flake_lock;
pub mod git;
pub mod secret;
pub mod ssh_key;
pub mod store;

pub use self::build_log::{
    replace_nix_log_pointer_with_web_log_link, strip_ansi_escape_sequences, strip_nix_log_tail,
};
pub use self::cache_key::*;
pub use self::daemon::*;
pub use self::error::StoreDaemonError;
pub use self::evaluator::*;
pub use self::git::{
    BranchHead, HeadCommit, check_task_updates, checkout_commit, checkout_commit_into,
    get_commit_info, head_commit, remote_branch_head, remote_tag_commit, resolve_remote_ref,
};
pub use self::secret::{decrypt_secret, encrypt_secret};
pub use self::ssh_key::{decrypt_ssh_private_key, format_public_key, generate_ssh_key};
pub use self::store::*;

use thiserror::Error;

pub fn cache_key_host(serve_url: &str) -> String {
    serve_url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-")
}

pub fn full_signature_token(sig_bytes: &[u8], serve_url: &str, cache_name: &str) -> String {
    use base64::{Engine, engine::general_purpose};
    format!(
        "{}-{}:{}",
        cache_key_host(serve_url),
        cache_name,
        general_purpose::STANDARD.encode(sig_bytes)
    )
}

#[derive(Debug, Clone, Error)]
pub enum SourceError {
    #[error("Failed to read file: {reason}")]
    FileRead { reason: String },
    #[error("Invalid SSH key format")]
    InvalidSshKey,
    #[error("SSH key generation failed")]
    SshKeyGeneration,
    #[error("Git command failed: {0}")]
    GitCommand(String),
    #[error("Invalid URL format")]
    InvalidUrl,
    #[error("Missing required hash in URL")]
    MissingHash,
    #[error("Invalid path format")]
    InvalidPath,
    #[error("Input validation failed: {reason}")]
    InputValidation { reason: String },
    #[error("Failed to parse JSON: {reason}")]
    JsonParsing { reason: String },
    #[error("Signing key operation failed")]
    SigningKeyOperation,
    #[error("Cryptographic operation failed")]
    CryptographicOperation,
    #[error("Failed to decode project '{project}' private key: {reason}")]
    ProjectKeyDecoding { project: String, reason: String },
    #[error("Failed to convert decrypted private key to UTF-8")]
    KeyUtf8Conversion,
    #[error("Failed to decrypt private key for project '{project}'")]
    KeyDecryption { project: String },
    #[error("Failed to decode cache '{cache}' signing key: {reason}")]
    CacheKeyDecoding { cache: String, reason: String },
    #[error("Failed to decrypt private key")]
    PrivateKeyDecryption,
    #[error("Failed to convert decrypted private key to KeyPair")]
    KeyPairConversion,
    #[error("Nix daemon connection failed")]
    NixDaemonConnection,
    #[error("Nix operation failed: {reason}")]
    NixOperation { reason: String },
    #[error("Database operation failed: {reason}")]
    Database { reason: String },
    #[error("Git command failed: {stderr}")]
    GitCommandFailed { stderr: String },
    #[error("Git command execution failed: {error}")]
    GitExecution { error: String },
    #[error("Failed to parse git output as UTF-8")]
    GitOutputParsing,
    #[error("Insufficient commit information returned from git")]
    InsufficientCommitInfo,
    #[error("URL parsing failed")]
    UrlParsing,
    #[error("branch {branch} does not exist on the remote")]
    BranchNotFound { branch: String },
    #[error("Tag {tag} does not exist on the remote.")]
    TagNotFound { tag: String },
    #[error("the remote has no branches")]
    EmptyRemote,
    #[error("Project not found with ID: {id}")]
    ProjectNotFound { id: gradient_types::ids::ProjectId },
}

impl SourceError {
    pub fn short_reason_for_user(&self) -> String {
        match self {
            Self::GitCommand(message)
            | Self::GitCommandFailed { stderr: message }
            | Self::GitExecution { error: message } => {
                readable_reason_for_git_transport_error(message).to_owned()
            }
            Self::BranchNotFound { .. }
            | Self::TagNotFound { .. }
            | Self::EmptyRemote
            | Self::InvalidUrl
            | Self::UrlParsing => self.to_string(),
            _ => "Gradient failed to check the repository. Details live in the server log."
                .to_owned(),
        }
    }
}

/// All network failures share the same reason, so the answer can't reveal which ports are open.
fn readable_reason_for_git_transport_error(message: &str) -> &'static str {
    let message = message.to_lowercase();
    let mentions = |patterns: &[&str]| patterns.iter().any(|p| message.contains(p));
    if mentions(&[
        "repository not found",
        "could not be found",
        "does not appear to be a git repository",
        "not exported",
        "status code: 404",
        "returned error: 404",
    ]) {
        "The server has no repository at this URL."
    } else if mentions(&[
        "permission denied",
        "authentication",
        "could not read username",
        "access denied",
        "unauthorized",
        "forbidden",
        "publickey",
        "status code: 401",
        "status code: 403",
        "returned error: 401",
        "returned error: 403",
    ]) {
        "The server denied access. Check that the project's SSH key or access token can read the repository."
    } else {
        "Gradient can't list branches of the Git repository at this URL. Check the URL and port. Make sure Gradient can reach the server."
    }
}

#[cfg(test)]
mod short_reason_tests {
    use super::SourceError;

    fn reason(stderr: &str) -> String {
        SourceError::GitCommandFailed {
            stderr: stderr.into(),
        }
        .short_reason_for_user()
    }

    #[test]
    fn refused_closed_and_timed_out_connections_share_one_reason_without_the_raw_error() {
        let refused = reason("Connection refused (os error 111)");
        for raw in [
            "failed to fill whole buffer",
            "connection timed out (os error 110)",
            "SSH to 10.0.0.5: Kex handshake failed",
            "failed to connect to 10.0.0.5: port 4013: Connection refused",
        ] {
            assert_eq!(reason(raw), refused, "{raw}");
        }
        assert!(!refused.contains("os error"), "{refused}");
    }

    #[test]
    fn missing_repositories_and_denied_access_get_their_own_reasons() {
        let missing = reason("remote: Repository not found.");
        let denied = reason("git@example.com: Permission denied (publickey).");
        assert_eq!(missing, "The server has no repository at this URL.");
        assert!(denied.starts_with("The server denied access"), "{denied}");
        assert_eq!(
            SourceError::BranchNotFound {
                branch: "main".into()
            }
            .short_reason_for_user(),
            "branch main does not exist on the remote"
        );
        assert!(
            !SourceError::Database {
                reason: "pool timed out".into()
            }
            .short_reason_for_user()
            .contains("pool")
        );
    }
}
