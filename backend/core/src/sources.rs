/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use base64::{Engine, engine::general_purpose};
use ed25519_compact::{KeyPair, SecretKey};
use entity::evaluation::EvaluationStatus;
use git2::{Direction, RemoteCallbacks};
use sea_orm::EntityTrait;
use ssh_key::{
    Algorithm, LineEnding, PrivateKey, private::Ed25519Keypair, private::Ed25519PrivateKey,
    private::KeypairData, public::Ed25519PublicKey,
};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tempfile::NamedTempFile;
use thiserror::Error;
use tracing::{debug, error, info, instrument};

use super::input::{check_repository_url_is_ssh, vec_to_hex};
use super::types::*;

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

/// List the remote HEAD ref without spawning a git process.
/// Uses libgit2 via the `git2` crate; SSH credentials are passed in-memory.
fn ls_remote_head(
    url: &str,
    private_key: Option<&str>,
    public_key: Option<&str>,
) -> Result<Vec<u8>, SourceError> {
    let mut remote =
        git2::Remote::create_detached(url).map_err(|e| SourceError::GitCommand(e.to_string()))?;

    let mut callbacks = RemoteCallbacks::new();
    callbacks.certificate_check(|_cert, _valid| {
        Ok(git2::CertificateCheckStatus::CertificateOk)
    });
    if let (Some(priv_key), Some(pub_key)) = (private_key, public_key) {
        let priv_key = priv_key.to_string();
        let pub_key = pub_key.to_string();
        callbacks.credentials(move |_url, username_from_url, _allowed| {
            git2::Cred::ssh_key_from_memory(
                username_from_url.unwrap_or("git"),
                Some(&pub_key),
                &priv_key,
                None,
            )
        });
    }

    let conn = remote
        .connect_auth(Direction::Fetch, Some(callbacks), None)
        .map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;

    let list = conn.list().map_err(|e| SourceError::GitCommandFailed {
        stderr: e.message().to_string(),
    })?;

    list.iter()
        .find(|h| h.name() == "HEAD")
        .or_else(|| list.first())
        .map(|h| h.oid().as_bytes().to_vec())
        .ok_or(SourceError::GitHashExtraction)
}

#[instrument(skip(state), fields(project_id = %project.id, project_name = %project.name))]
pub async fn check_project_updates(
    state: Arc<ServerState>,
    project: &MProject,
) -> Result<(bool, Vec<u8>), SourceError> {
    debug!("Checking for updates on project");

    let url = project.repository.clone();
    let ssh_creds: Option<(String, String)> = if check_repository_url_is_ssh(&url) {
        let organization = EOrganization::find_by_id(project.organization)
            .one(&state.db)
            .await
            .map_err(|e| SourceError::Database {
                reason: e.to_string(),
            })?
            .ok_or(SourceError::OrganizationNotFound {
                id: project.organization,
            })?;
        Some(decrypt_ssh_private_key(
            state.cli.crypt_secret_file.clone(),
            organization,
            &state.cli.serve_url,
        )?)
    } else {
        None
    };

    let remote_hash = match tokio::task::spawn_blocking(move || {
        if let Some((private_key, public_key)) = ssh_creds {
            ls_remote_head(&url, Some(&private_key), Some(&public_key))
        } else {
            ls_remote_head(&url, None, None)
        }
    })
    .await
    .map_err(|e| SourceError::GitExecution {
        error: e.to_string(),
    })? {
        Ok(hash) => hash,
        Err(e) => {
            error!(error = %e, "Failed to get remote HEAD ref");
            return Ok((false, vec![]));
        }
    };

    let remote_hash_str = vec_to_hex(&remote_hash);
    debug!(remote_hash = %remote_hash_str, "Retrieved remote hash");

    if project.force_evaluation {
        info!("Force evaluation enabled, updating project");
        return Ok((true, remote_hash));
    }

    if let Some(last_evaluation) = project.last_evaluation {
        let evaluation = EEvaluation::find_by_id(last_evaluation)
            .one(&state.db)
            .await
            .map_err(|e| SourceError::Database {
                reason: e.to_string(),
            })?
            .ok_or_else(|| SourceError::Database {
                reason: "Evaluation not found".to_string(),
            })?;

        if evaluation.status == EvaluationStatus::Queued
            || evaluation.status == EvaluationStatus::Evaluating
            || evaluation.status == EvaluationStatus::Building
        {
            debug!(status = ?evaluation.status, "Evaluation already in progress, skipping");
            return Ok((false, remote_hash));
        }

        let commit = ECommit::find_by_id(evaluation.commit)
            .one(&state.db)
            .await
            .map_err(|e| SourceError::Database {
                reason: e.to_string(),
            })?
            .ok_or_else(|| SourceError::Database {
                reason: "Commit not found".to_string(),
            })?;

        if commit.hash == remote_hash {
            debug!("Remote hash matches current evaluation commit, no update needed");
            return Ok((false, remote_hash));
        }

        info!("Remote hash differs from current evaluation commit, update needed");
    } else {
        info!("No previous evaluation found, update needed");
    }

    Ok((true, remote_hash))
}

#[instrument(skip(state), fields(project_id = %project.id, project_name = %project.name, commit_hash = %vec_to_hex(commit_hash)))]
pub async fn get_commit_info(
    state: Arc<ServerState>,
    project: &MProject,
    commit_hash: &[u8],
) -> Result<(String, Option<String>, String), SourceError> {
    debug!("Fetching commit info");

    let hash_str = vec_to_hex(commit_hash);
    let url = project.repository.clone();

    let ssh_creds: Option<(String, String)> = if check_repository_url_is_ssh(&url) {
        let organization = EOrganization::find_by_id(project.organization)
            .one(&state.db)
            .await
            .map_err(|e| SourceError::Database {
                reason: e.to_string(),
            })?
            .ok_or(SourceError::OrganizationNotFound {
                id: project.organization,
            })?;
        Some(decrypt_ssh_private_key(
            state.cli.crypt_secret_file.clone(),
            organization,
            &state.cli.serve_url,
        )?)
    } else {
        None
    };

    let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
        reason: e.to_string(),
    })?;
    let temp_path = temp_dir.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut callbacks = RemoteCallbacks::new();
        callbacks.certificate_check(|_cert, _valid| {
            Ok(git2::CertificateCheckStatus::CertificateOk)
        });
        if let Some((private_key, public_key)) = ssh_creds {
            callbacks.credentials(move |_url, username_from_url, _allowed| {
                git2::Cred::ssh_key_from_memory(
                    username_from_url.unwrap_or("git"),
                    Some(&public_key),
                    &private_key,
                    None,
                )
            });
        }

        let mut fo = git2::FetchOptions::new();
        fo.remote_callbacks(callbacks);

        let mut builder = git2::build::RepoBuilder::new();
        builder.bare(true);
        builder.fetch_options(fo);
        let repo = builder
            .clone(&url, &temp_path)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        let oid = git2::Oid::from_str(&hash_str).map_err(|_| SourceError::GitOutputParsing)?;
        let commit = repo
            .find_commit(oid)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        let message = commit.summary().unwrap_or("").to_string();
        let author_email = commit.author().email().map(|s| s.to_string());
        let author_name = commit.author().name().unwrap_or("").to_string();

        Ok((message, author_email, author_name))
    })
    .await
    .map_err(|e| SourceError::GitExecution {
        error: e.to_string(),
    })?
}

pub fn write_key(private_key: String) -> Result<String, SourceError> {
    let mut temp_file = NamedTempFile::with_suffix(".key").map_err(|e| SourceError::FileRead {
        reason: e.to_string(),
    })?;

    let path = temp_file.path().to_string_lossy().to_string();

    fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o600))
        .map_err(|_| SourceError::KeyFilePermissions { path: path.clone() })?;

    temp_file
        .write_all(private_key.as_bytes())
        .map_err(|_| SourceError::KeyFileWrite { path: path.clone() })?;

    temp_file
        .keep()
        .map_err(|_| SourceError::KeyFileWrite { path: path.clone() })?;

    Ok(path)
}

pub fn clear_key(path: String) -> Result<(), SourceError> {
    fs::remove_file(&path).map_err(|_| SourceError::KeyFileRemoval { path })?;
    Ok(())
}

/// Parses a nix flake URL of the form `git+<scheme>://host/repo?rev=<hash>` into
/// `(git_url, rev)`.  The `git+` prefix is stripped so the returned URL is
/// suitable for direct use with libgit2.
fn parse_nix_git_url(nix_url: &str) -> Result<(String, String), SourceError> {
    let url = nix_url.strip_prefix("git+").unwrap_or(nix_url);
    let (base_url, query) = url.split_once('?').ok_or(SourceError::UrlParsing)?;
    let rev = query
        .split('&')
        .find_map(|p| p.strip_prefix("rev="))
        .ok_or(SourceError::MissingHash)?
        .to_string();
    Ok((base_url.to_string(), rev))
}

/// Fetches the repository into a local temporary directory using libgit2 and
/// returns a handle to that directory.
///
/// For **SSH repositories** the repo is cloned via libgit2 (using in-memory
/// SSH credentials) and the checked-out working tree is returned so that the
/// caller can pass `path:<dir>` to the Nix C API, which avoids any further
/// SSH interaction.
///
/// For **HTTPS repositories** `None` is returned; the Nix evaluator will
/// fetch the flake on demand via `builtins.getFlake`.
#[instrument(skip(state, organization), fields(repository = %repository))]
pub async fn prefetch_flake(
    state: Arc<ServerState>,
    repository: String,
    organization: MOrganization,
) -> Result<Option<tempfile::TempDir>, SourceError> {
    if !check_repository_url_is_ssh(&repository) {
        debug!("HTTPS repository – skipping git clone, nix will fetch on demand");
        return Ok(None);
    }

    debug!("SSH repository – cloning via libgit2: {}", repository);

    let (private_key, public_key) =
        decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization, &state.cli.serve_url)?;

    let (git_url, rev) = parse_nix_git_url(&repository)?;

    let temp_dir = tempfile::TempDir::new().map_err(|e| SourceError::FileRead {
        reason: e.to_string(),
    })?;
    let temp_path = temp_dir.path().to_path_buf();

    tokio::task::spawn_blocking(move || {
        let mut callbacks = RemoteCallbacks::new();
        callbacks.certificate_check(|_cert, _valid| {
            Ok(git2::CertificateCheckStatus::CertificateOk)
        });
        let priv_key = private_key.clone();
        let pub_key = public_key.clone();
        callbacks.credentials(move |_url, username_from_url, _allowed| {
            git2::Cred::ssh_key_from_memory(
                username_from_url.unwrap_or("git"),
                Some(&pub_key),
                &priv_key,
                None,
            )
        });

        let mut fo = git2::FetchOptions::new();
        fo.remote_callbacks(callbacks);

        let repo = git2::build::RepoBuilder::new()
            .fetch_options(fo)
            .clone(&git_url, &temp_path)
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;

        let oid =
            git2::Oid::from_str(&rev).map_err(|_| SourceError::GitOutputParsing)?;
        let commit = repo.find_commit(oid).map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;
        let tree = commit.tree().map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;

        let mut co = git2::build::CheckoutBuilder::new();
        co.force();
        repo.checkout_tree(tree.as_object(), Some(&mut co))
            .map_err(|e| SourceError::GitCommandFailed {
                stderr: e.message().to_string(),
            })?;
        repo.set_head_detached(oid).map_err(|e| SourceError::GitCommandFailed {
            stderr: e.message().to_string(),
        })?;

        debug!("Cloned repository to {:?} at rev {}", temp_path, rev);

        // Lock the flake via the Nix C API. This drives libfetchers in-process,
        // which fetches every transitive input (including private git+ssh ones,
        // because we surface the org SSH key via GIT_SSH_COMMAND inside
        // `lock_flake_with_ssh_key`) into /nix/store before evaluation runs.
        crate::nix_flake::lock_flake_with_ssh_key(&temp_path, &private_key).map_err(|e| {
            SourceError::NixFlakeArchiveFailed {
                stderr: e.to_string(),
            }
        })?;

        debug!("Locked flake and prefetched inputs for {:?}", temp_path);

        Ok::<(), SourceError>(())
    })
    .await
    .map_err(|e| SourceError::GitExecution { error: e.to_string() })??;

    Ok(Some(temp_dir))
}

pub fn generate_ssh_key(secret_file: String) -> Result<(String, String), SourceError> {
    let secret = crate::input::load_secret_bytes(&secret_file);

    let keypair = KeyPair::generate();

    let public_key_bytes: [u8; 32] = keypair
        .pk
        .as_slice()
        .try_into()
        .map_err(|_| SourceError::SshKeyGeneration)?;
    let private_key_bytes: [u8; 32] = keypair
        .sk
        .seed()
        .as_slice()
        .try_into()
        .map_err(|_| SourceError::SshKeyGeneration)?;

    let keypair_data = KeypairData::Ed25519(Ed25519Keypair {
        public: Ed25519PublicKey::try_from(&public_key_bytes[..])
            .map_err(|_| SourceError::SshKeyGeneration)?,
        private: Ed25519PrivateKey::from_bytes(&private_key_bytes),
    });
    let private_key =
        PrivateKey::new(keypair_data, "").map_err(|_| SourceError::SshKeyGeneration)?;

    let private_key_openssh = private_key
        .to_openssh(LineEnding::LF)
        .map_err(|_| SourceError::SshKeyGeneration)?
        .to_string();

    let public_key_parts = private_key
        .public_key()
        .to_openssh()
        .map_err(|_| SourceError::SshKeyGeneration)?
        .to_string();

    let public_key_data = public_key_parts
        .split_whitespace()
        .nth(1)
        .ok_or(SourceError::InvalidSshKey)?;

    let public_key_openssh = format!("{} {}", Algorithm::Ed25519.as_str(), public_key_data);

    let encrypted_private_key = crypter::encrypt_with_password(&secret, &private_key_openssh)
        .ok_or(SourceError::CryptographicOperation)?;

    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok((encrypted_private_key, public_key_openssh))
}

pub fn decrypt_ssh_private_key(
    secret_file: String,
    organization: MOrganization,
    serve_url: &str,
) -> Result<(String, String), SourceError> {
    let secret = crate::input::load_secret_bytes(&secret_file);

    let encrypted_private_key = general_purpose::STANDARD
        .decode(organization.clone().private_key)
        .map_err(|e| SourceError::OrganizationKeyDecoding {
            org: organization.name.clone(),
            reason: format!("{}. The private key in the database appears to be corrupted or not properly base64-encoded.", e)
        })?;

    let decrypted_private_key = if let Some(p) =
        crypter::decrypt_with_password(&secret, encrypted_private_key.clone())
    {
        String::from_utf8(p).map_err(|_| SourceError::KeyUtf8Conversion)?
    } else {
        tracing::warn!(
            "Failed to decrypt private key for organization '{}', attempting to decode as plaintext base64",
            organization.name
        );
        match String::from_utf8(encrypted_private_key) {
            Ok(plaintext) => {
                if plaintext.starts_with("-----BEGIN") {
                    tracing::warn!(
                        "Organization '{}' private key appears to be stored as plaintext base64",
                        organization.name
                    );
                    plaintext
                } else {
                    return Err(SourceError::KeyDecryption {
                        org: organization.name.clone(),
                    });
                }
            }
            Err(_) => {
                return Err(SourceError::KeyDecryption {
                    org: organization.name.clone(),
                });
            }
        }
    };

    let formatted_public_key = format_public_key(organization, serve_url);
    let decrypted_private_key = format!("{}\n", decrypted_private_key);

    Ok((decrypted_private_key, formatted_public_key))
}

pub fn format_public_key(organization: MOrganization, serve_url: &str) -> String {
    let hostname = serve_url
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split('/')
        .next()
        .unwrap_or(serve_url);
    format!("{} {}-{}", organization.public_key, hostname, organization.name)
}

/// Returns `(encrypted_private_key, public_key_b64)`.
/// The private key is the full 64-byte ed25519 keypair encrypted and base64-encoded.
/// The public key is the last 32 bytes of the keypair, base64-encoded in plaintext.
pub fn generate_signing_key(secret_file: String) -> Result<(String, String), SourceError> {
    let secret = crate::input::load_secret_bytes(&secret_file);

    let keypair = KeyPair::generate();
    // Base64-encode the full 64-byte keypair (seed || public key)
    let key_b64 = general_purpose::STANDARD.encode(*keypair);
    // Derive the standalone public key (last 32 bytes)
    let public_key_b64 = general_purpose::STANDARD.encode(*keypair.pk);

    let encrypted_private_key = crypter::encrypt_with_password(&secret, key_b64.as_bytes())
        .ok_or(SourceError::CryptographicOperation)?;

    Ok((
        general_purpose::STANDARD.encode(&encrypted_private_key),
        public_key_b64,
    ))
}

pub fn format_cache_public_key(
    secret_file: String,
    cache: MCache,
    url: String,
) -> Result<String, SourceError> {
    // Use the stored public key when available; fall back to deriving it from
    // the encrypted private key for caches created before the split migration.
    let pubkey_b64 = if cache.public_key.is_empty() {
        let key_b64 = decrypt_signing_key(secret_file, cache.clone())?;
        let key_bytes = general_purpose::STANDARD
            .decode(key_b64.trim())
            .map_err(|e| SourceError::CacheKeyDecoding {
                cache: cache.name.clone(),
                reason: format!("Failed to base64-decode signing key: {}", e),
            })?;
        if key_bytes.len() < 32 {
            return Err(SourceError::KeyPairConversion);
        }
        general_purpose::STANDARD.encode(&key_bytes[key_bytes.len() - 32..])
    } else {
        cache.public_key.clone()
    };

    let base_url = url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    Ok(format!("{}-{}:{}", base_url, cache.name, pubkey_b64))
}

pub fn decrypt_signing_key(secret_file: String, cache: MCache) -> Result<String, SourceError> {
    let secret = crate::input::load_secret_bytes(&secret_file);

    let encrypted_private_key = general_purpose::STANDARD
        .decode(cache.clone().private_key)
        .map_err(|e| SourceError::CacheKeyDecoding {
            cache: cache.name.clone(),
            reason: format!("{}. The private key in the cache appears to be corrupted or not properly base64-encoded.", e)
        })?;

    let decrypted_private_key = crypter::decrypt_with_password(&secret, encrypted_private_key)
        .ok_or(SourceError::PrivateKeyDecryption)?;

    // Convert decrypted bytes to string (signing key should be base64)
    let decrypted_key_str =
        String::from_utf8(decrypted_private_key).map_err(|_| SourceError::KeyUtf8Conversion)?;

    Ok(decrypted_key_str)
}

pub fn format_cache_key(
    secret_file: String,
    cache: MCache,
    url: String,
) -> Result<String, SourceError> {
    let decrypted_key = decrypt_signing_key(secret_file, cache.clone())?;

    let base_url = url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    Ok(format!(
        "{}-{}:{}",
        base_url,
        cache.name,
        decrypted_key.trim()
    ))
}

/// Signs a Nix narinfo fingerprint directly with the cache's Ed25519 key.
///
/// Fingerprint format: `1;{store_path};{nar_hash};{nar_size};{refs_sorted_comma}`
/// Returns a full signature token: `{key_name}:{base64_sig}`.
///
/// References should be bare store-path names (without `/nix/store/` prefix);
/// this function adds the prefix before sorting and joining.
pub fn sign_narinfo_fingerprint(
    secret_file: String,
    cache: MCache,
    serve_url: String,
    store_path: &str,
    nar_hash: &str,
    nar_size: u64,
    references: &[String],
) -> Result<String, SourceError> {
    let key_b64 = decrypt_signing_key(secret_file, cache.clone())?;
    let key_bytes = general_purpose::STANDARD
        .decode(key_b64.trim())
        .map_err(|e| SourceError::CacheKeyDecoding {
            cache: cache.name.clone(),
            reason: format!("Failed to base64-decode signing key: {}", e),
        })?;

    let secret_key = SecretKey::from_slice(&key_bytes)
        .map_err(|_| SourceError::KeyPairConversion)?;

    // Nix fingerprint uses full store paths for references, sorted lexicographically.
    let mut full_refs: Vec<String> = references
        .iter()
        .map(|r| {
            if r.starts_with("/nix/store/") {
                r.clone()
            } else {
                format!("/nix/store/{}", r)
            }
        })
        .collect();
    full_refs.sort();
    let refs_str = full_refs.join(",");

    let fingerprint = format!("1;{};{};{};{}", store_path, nar_hash, nar_size, refs_str);
    let sig = secret_key.sign(fingerprint.as_bytes(), None);
    let sig_b64 = general_purpose::STANDARD.encode(*sig);

    let base_url = serve_url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    Ok(format!("{}-{}:{}", base_url, cache.name, sig_b64))
}

pub fn get_hash_from_url(url: String) -> Result<String, SourceError> {
    let path_split = url.split('.').collect::<Vec<&str>>();

    // Check if we have exactly 2 or 3 parts (hash.extension[.compression])
    if !(path_split.len() == 2 || path_split.len() == 3) {
        return Err(SourceError::InvalidPath);
    }

    // Accept 32-char store-path hashes (160-bit nix32) and 52-char file/nar hashes (256-bit nix32)
    if path_split[0].len() != 32 && path_split[0].len() != 52 {
        return Err(SourceError::InvalidPath);
    }

    // Check extension
    if !((path_split[1] == "narinfo" && path_split.len() == 2) || path_split[1] == "nar") {
        return Err(SourceError::InvalidPath);
    }

    // Check hash characters (base32) - exclude 'e', 'o', 't', 'u'
    if !path_split[0]
        .chars()
        .all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c))
    {
        return Err(SourceError::InvalidPath);
    }

    Ok(path_split[0].to_string())
}

pub fn get_hash_from_path(path: String) -> Result<(String, String), SourceError> {
    let path_split = path.split('/').collect::<Vec<&str>>();
    if path_split.len() < 4 {
        return Err(SourceError::InvalidPath);
    }

    let path_split = path_split[3].split('-').collect::<Vec<&str>>();
    if path_split.len() < 2 {
        return Err(SourceError::InvalidPath);
    }

    let package = path_split[1..].join("-");
    let hash = path_split[0].to_string();

    Ok((hash, package))
}

pub fn get_path_from_build_output(build_output: MBuildOutput) -> String {
    format!("/nix/store/{}-{}", build_output.hash, build_output.package)
}

pub fn get_cache_nar_location(base_path: String, hash: String) -> Result<String, SourceError> {
    let hash_hex = hash.as_str();
    std::fs::create_dir_all(format!("{}/nars/{}", base_path, &hash_hex[0..2])).map_err(|e| {
        SourceError::FileRead {
            reason: e.to_string(),
        }
    })?;

    Ok(format!(
        "{}/nars/{}/{}.nar",
        base_path,
        &hash_hex[0..2],
        &hash_hex[2..],
    ))
}

/// Returns the on-disk path for a compressed (zstd) NAR cache file.
/// Used for non-entry-point builds that are cached on first serve.
pub fn get_cache_nar_compressed_location(base_path: String, hash: String) -> Result<String, SourceError> {
    let hash_hex = hash.as_str();
    std::fs::create_dir_all(format!("{}/nars/{}", base_path, &hash_hex[0..2])).map_err(|e| {
        SourceError::FileRead {
            reason: e.to_string(),
        }
    })?;

    Ok(format!(
        "{}/nars/{}/{}.nar.zst",
        base_path,
        &hash_hex[0..2],
        &hash_hex[2..],
    ))
}
