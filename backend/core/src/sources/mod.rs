/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub mod cache_key;
pub mod git;
pub mod nar_path;
pub mod ssh_key;

pub use self::cache_key::*;
pub use self::git::{check_project_updates, get_commit_info, Libgit2Prefetcher};
pub use self::nar_path::*;
pub use self::ssh_key::{clear_key, decrypt_ssh_private_key, format_public_key, generate_ssh_key, write_key};

use anyhow::Result;
use async_trait::async_trait;
use crate::types::*;
use std::path::PathBuf;
use thiserror::Error;

#[derive(Debug, Clone, Error)]
pub enum SourceError {
    #[error("Failed to read file: {reason}")]
    FileRead { reason: String },
    #[error("Failed to write key file: {path}")]
    KeyFileWrite { path: String },
    #[error("Failed to set key file permissions: {path}")]
    KeyFilePermissions { path: String },
    #[error("Failed to remove key file: {path}")]
    KeyFileRemoval { path: String },
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
    #[error("Failed to decode organization '{org}' private key: {reason}")]
    OrganizationKeyDecoding { org: String, reason: String },
    #[error("Failed to convert decrypted private key to UTF-8")]
    KeyUtf8Conversion,
    #[error("Failed to decrypt private key for organization '{org}'")]
    KeyDecryption { org: String },
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
    #[error("Nix command not found or not in PATH")]
    NixNotFound,
    #[error("SSH authentication failed for flake input")]
    FlakeSSHAuth,
    #[error("Network connection failed while fetching flake inputs")]
    FlakeNetworkConnection,
    #[error("Nix flake archive failed: {stderr}")]
    NixFlakeArchiveFailed { stderr: String },
    #[error("URL parsing failed")]
    UrlParsing,
    #[error("Unable to extract hash from Git URL")]
    GitHashExtraction,
    #[error("Organization not found with ID: {id}")]
    OrganizationNotFound { id: uuid::Uuid },
}

/// Result of a successful prefetch. Owns the temporary clone directory so the
/// caller keeps it alive for as long as the path is used.
#[derive(Debug)]
pub struct PrefetchedFlake {
    _dir: tempfile::TempDir,
    pub path: PathBuf,
}

impl PrefetchedFlake {
    pub fn from_tempdir(dir: tempfile::TempDir) -> Self {
        let path = dir.path().to_path_buf();
        Self { _dir: dir, path }
    }
}

/// Prefetches a flake repository for evaluation. Production impl uses libgit2
/// + the Nix C API to clone SSH repos and lock their inputs into the store;
/// tests can substitute a fake that returns `None` or a stub directory.
#[async_trait]
pub trait FlakePrefetcher: Send + Sync + std::fmt::Debug + 'static {
    async fn prefetch(
        &self,
        crypt_secret_file: String,
        serve_url: String,
        repository: String,
        organization: MOrganization,
    ) -> Result<Option<PrefetchedFlake>>;
}
