/*
 * SPDX-FileCopyrightText: 2025 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use base64::{Engine, engine::general_purpose};
use ed25519_compact::KeyPair;
use entity::evaluation::EvaluationStatus;
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
use tokio::process::Command;
use tracing::{debug, error, info, instrument};

use super::input::{check_repository_url_is_ssh, hex_to_vec, load_secret, vec_to_hex};
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
    #[error("Invalid base64 data")]
    InvalidBase64,
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
    #[error("Failed to decode GRADIENT_CRYPT_SECRET from file '{file}': {reason}")]
    SecretDecoding { file: String, reason: String },
    #[error(
        "GRADIENT_CRYPT_SECRET is too short ({length} bytes). Encryption keys should be at least 16 bytes"
    )]
    SecretTooShort { length: usize },
    #[error(
        "GRADIENT_CRYPT_SECRET has invalid length ({length} bytes). AES-256 requires exactly {expected} bytes"
    )]
    SecretInvalidLength { length: usize, expected: usize },
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

#[instrument(skip(state), fields(project_id = %project.id, project_name = %project.name))]
pub async fn check_project_updates(
    state: Arc<ServerState>,
    project: &MProject,
) -> Result<(bool, Vec<u8>), SourceError> {
    debug!("Checking for updates on project");

    let cmd = match if check_repository_url_is_ssh(&project.repository) {
        let organization = EOrganization::find_by_id(project.organization)
            .one(&state.db)
            .await
            .map_err(|e| SourceError::Database {
                reason: e.to_string(),
            })?
            .ok_or(SourceError::OrganizationNotFound {
                id: project.organization,
            })?;

        let (private_key, _public_key) =
            decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization)?;

        let ssh_key_path = write_key(private_key)?;

        let cmd = Command::new(state.cli.binpath_git.clone())
            .arg("-c")
            .arg(format!("core.sshCommand={} -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null", state.cli.binpath_ssh, ssh_key_path))
            .arg("ls-remote")
            .arg(&project.repository)
            .output()
            .await;

        clear_key(ssh_key_path)?;

        cmd
    } else {
        Command::new(state.cli.binpath_git.clone())
            .arg("ls-remote")
            .arg(&project.repository)
            .output()
            .await
    } {
        Ok(output) => output,
        Err(e) => {
            error!(error = %e, "Failed to execute git ls-remote command");
            return Ok((false, vec![]));
        }
    };

    if !cmd.status.success() {
        let errmsg = String::from_utf8(cmd.stderr).map_err(|_| SourceError::GitOutputParsing)?;
        if errmsg.contains("cannot run ssh: No such file or directory") {
            error!(stderr = %errmsg, "SSH binary not found. Please ensure OpenSSH client is available in PATH or set GRADIENT_BINPATH_SSH");
        } else if errmsg.contains("Permission denied")
            || errmsg.contains("Host key verification failed")
        {
            error!(stderr = %errmsg, "SSH authentication failed. Please check SSH key configuration");
        } else if errmsg.contains("Connection refused") || errmsg.contains("Network is unreachable")
        {
            error!(stderr = %errmsg, "SSH connection failed. Please check repository URL and network connectivity");
        } else {
            error!(stderr = %errmsg, "Git ls-remote command failed");
        }
        return Ok((false, vec![]));
    }

    let output = String::from_utf8(cmd.stdout).map_err(|_| SourceError::GitOutputParsing)?;
    let output = output.lines().collect::<Vec<&str>>()[0]
        .split_whitespace()
        .collect::<Vec<&str>>();

    if output.len() != 2 {
        error!("Invalid git ls-remote output format: expected hash and ref");
        return Ok((false, vec![]));
    }

    let remote_hash = match hex_to_vec(output[0]) {
        Ok(hash) => hash,
        Err(e) => {
            error!(error = %e, "Failed to parse remote hash");
            return Ok((false, vec![]));
        }
    };
    let remote_hash_str = output[0];
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
    let temp_dir = format!(
        "{}/temp_clone_{}",
        state.cli.base_path,
        uuid::Uuid::new_v4()
    );

    let clone_cmd = if check_repository_url_is_ssh(&project.repository) {
        let organization = EOrganization::find_by_id(project.organization)
            .one(&state.db)
            .await
            .map_err(|e| SourceError::Database {
                reason: e.to_string(),
            })?
            .ok_or(SourceError::OrganizationNotFound {
                id: project.organization,
            })?;

        let (private_key, _public_key) =
            decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization)?;

        let ssh_key_path = write_key(private_key)?;

        let cmd = Command::new(state.cli.binpath_git.clone())
            .arg("clone")
            .arg("--bare")
            .arg("-c")
            .arg(format!("core.sshCommand={} -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null", state.cli.binpath_ssh, ssh_key_path))
            .arg(&project.repository)
            .arg(&temp_dir)
            .output()
            .await;

        clear_key(ssh_key_path)?;

        cmd
    } else {
        Command::new(state.cli.binpath_git.clone())
            .arg("clone")
            .arg("--bare")
            .arg(&project.repository)
            .arg(&temp_dir)
            .output()
            .await
    };

    match clone_cmd {
        Ok(output) => {
            if !output.status.success() {
                let err = String::from_utf8_lossy(&output.stderr);
                return Err(SourceError::GitCommandFailed {
                    stderr: err.to_string(),
                });
            }
        }
        Err(e) => {
            return Err(SourceError::GitExecution {
                error: e.to_string(),
            });
        }
    }

    let show_cmd = Command::new(state.cli.binpath_git.clone())
        .arg("show")
        .arg("--format=%s%n%ae%n%an")
        .arg("--no-patch")
        .arg(&hash_str)
        .current_dir(&temp_dir)
        .output()
        .await;

    let result = match show_cmd {
        Ok(output) => {
            if !output.status.success() {
                let err = String::from_utf8_lossy(&output.stderr);
                Err(SourceError::GitCommandFailed {
                    stderr: err.to_string(),
                })
            } else {
                let stdout =
                    String::from_utf8(output.stdout).map_err(|_| SourceError::GitOutputParsing)?;
                let lines: Vec<&str> = stdout.lines().collect();

                if lines.len() < 3 {
                    Err(SourceError::InsufficientCommitInfo)
                } else {
                    let message = lines[0].to_string();
                    let author_email = if lines[1].is_empty() {
                        None
                    } else {
                        Some(lines[1].to_string())
                    };
                    let author_name = lines[2].to_string();
                    Ok((message, author_email, author_name))
                }
            }
        }
        Err(e) => Err(SourceError::GitExecution {
            error: e.to_string(),
        }),
    };

    std::fs::remove_dir_all(&temp_dir).ok();

    result
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

#[instrument(skip(state, organization), fields(repository = %repository))]
pub async fn prefetch_flake(
    state: Arc<ServerState>,
    repository: String,
    organization: MOrganization,
) -> Result<(), SourceError> {
    debug!("Prefetching flake inputs for repository: {}", repository);

    let (private_key, _public_key) =
        decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization)?;

    let ssh_key_path = write_key(private_key)?;

    let cmd = Command::new(state.cli.binpath_nix.clone())
        .arg("flake")
        .arg("archive")
        .arg(&repository)
        .env("GIT_SSH_COMMAND", format!("{} -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null", state.cli.binpath_ssh, ssh_key_path))
        .output()
        .await;

    clear_key(ssh_key_path).ok(); // Clean up SSH key

    let cmd = match cmd {
        Ok(output) => output,
        Err(e) => {
            error!(error = %e, "Failed to execute nix flake archive command");
            return Err(SourceError::GitExecution {
                error: e.to_string(),
            });
        }
    };

    if !cmd.status.success() {
        let stderr = String::from_utf8_lossy(&cmd.stderr);
        error!(stderr = %stderr, "Nix flake archive command failed");

        if stderr.contains("command not found") || stderr.contains("No such file") {
            return Err(SourceError::NixNotFound);
        } else if stderr.contains("Permission denied") || stderr.contains("authentication failed") {
            return Err(SourceError::FlakeSSHAuth);
        } else if stderr.contains("Connection refused") || stderr.contains("Network is unreachable")
        {
            return Err(SourceError::FlakeNetworkConnection);
        } else {
            return Err(SourceError::NixFlakeArchiveFailed {
                stderr: stderr.to_string(),
            });
        }
    }

    let stdout = String::from_utf8_lossy(&cmd.stdout);
    debug!(stdout = %stdout, "Nix flake archive completed successfully");

    Ok(())
}

pub fn generate_ssh_key(secret_file: String) -> Result<(String, String), SourceError> {
    let secret = general_purpose::STANDARD
        .decode(load_secret(&secret_file))
        .map_err(|_| SourceError::InvalidBase64)?;

    // crypter 0.3 requires exactly 32 bytes for AES-256
    let secret_key: &[u8; 32] = secret.as_slice().try_into().map_err(|_| SourceError::SecretInvalidLength {
        length: secret.len(),
        expected: 32,
    })?;

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

    let encrypted_private_key = crypter::encrypt(secret_key, &private_key_openssh)
        .ok_or(SourceError::CryptographicOperation)?;

    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok((encrypted_private_key, public_key_openssh))
}

pub fn decrypt_ssh_private_key(
    secret_file: String,
    organization: MOrganization,
) -> Result<(String, String), SourceError> {
    let secret_content = crate::input::load_secret(&secret_file);

    let secret = general_purpose::STANDARD
        .decode(&secret_content)
        .map_err(|e| SourceError::SecretDecoding {
            file: secret_file.clone(),
            reason: format!(
                "{}. Please check that the file contains valid base64-encoded data.",
                e
            ),
        })?;

    if secret.len() < 16 {
        return Err(SourceError::SecretTooShort {
            length: secret.len(),
        });
    }

    let encrypted_private_key = general_purpose::STANDARD
        .decode(organization.clone().private_key)
        .map_err(|e| SourceError::OrganizationKeyDecoding {
            org: organization.name.clone(),
            reason: format!("{}. The private key in the database appears to be corrupted or not properly base64-encoded.", e)
        })?;

    // crypter 0.3 expects a reference to a 32-byte array for AES-256
    let secret_key: &[u8; 32] = secret.as_slice().try_into().map_err(|_| SourceError::SecretInvalidLength {
        length: secret.len(),
        expected: 32,
    })?;

    let decrypted_private_key = if let Some(p) =
        crypter::decrypt(secret_key, encrypted_private_key.clone())
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

    let formatted_public_key = format_public_key(organization);
    let decrypted_private_key = format!("{}\n", decrypted_private_key);

    Ok((decrypted_private_key, formatted_public_key))
}

pub fn format_public_key(organization: MOrganization) -> String {
    format!("{} {}", organization.public_key, organization.id)
}

pub fn generate_signing_key(secret_file: String) -> Result<String, SourceError> {
    let secret = general_purpose::STANDARD
        .decode(load_secret(&secret_file))
        .map_err(|_| SourceError::InvalidBase64)?;

    // crypter 0.3 requires exactly 32 bytes for AES-256
    let secret_key: &[u8; 32] = secret.as_slice().try_into().map_err(|_| SourceError::SecretInvalidLength {
        length: secret.len(),
        expected: 32,
    })?;

    let private_key = KeyPair::generate();
    let encrypted_private_key =
        crypter::encrypt(secret_key, *private_key).ok_or(SourceError::CryptographicOperation)?;

    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok(encrypted_private_key)
}

pub fn decrypt_signing_key(secret_file: String, cache: MCache) -> Result<String, SourceError> {
    let secret_content = crate::input::load_secret(&secret_file);

    let secret = general_purpose::STANDARD
        .decode(&secret_content)
        .map_err(|e| SourceError::SecretDecoding {
            file: secret_file.clone(),
            reason: format!(
                "{}. Please check that the file contains valid base64-encoded data.",
                e
            ),
        })?;

    let encrypted_private_key = general_purpose::STANDARD
        .decode(cache.clone().signing_key)
        .map_err(|e| SourceError::CacheKeyDecoding {
            cache: cache.name.clone(),
            reason: format!("{}. The signing key in the cache appears to be corrupted or not properly base64-encoded.", e)
        })?;

    // crypter 0.3 requires exactly 32 bytes for AES-256
    let secret_key: &[u8; 32] = secret.as_slice().try_into().map_err(|_| SourceError::SecretInvalidLength {
        length: secret.len(),
        expected: 32,
    })?;

    let decrypted_private_key =
        crypter::decrypt(secret_key, encrypted_private_key).ok_or(SourceError::PrivateKeyDecryption)?;

    // Convert decrypted bytes to string (signing key should be base64)
    let decrypted_key_str = String::from_utf8(decrypted_private_key)
        .map_err(|_| SourceError::KeyUtf8Conversion)?;

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

    Ok(format!("{}-{}:{}", base_url, cache.name, decrypted_key.trim()))
}

pub fn get_hash_from_url(url: String) -> Result<String, SourceError> {
    let path_split = url.split('.').collect::<Vec<&str>>();

    // Check if we have exactly 2 or 3 parts (hash.extension[.compression])
    if !(path_split.len() == 2 || path_split.len() == 3) {
        return Err(SourceError::InvalidPath);
    }

    // Check hash length (32 characters)
    if path_split[0].len() != 32 {
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

pub fn get_cache_nar_location(
    base_path: String,
    hash: String,
    compressed: bool,
) -> Result<String, SourceError> {
    let hash_hex = hash.as_str();
    std::fs::create_dir_all(format!("{}/nars/{}", base_path, &hash_hex[0..2])).map_err(|e| {
        SourceError::FileRead {
            reason: e.to_string(),
        }
    })?;
    Ok(format!(
        "{}/nars/{}/{}.nar{}",
        base_path,
        &hash_hex[0..2],
        &hash_hex[2..],
        if compressed { ".zst" } else { "" }
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;
    use std::io::Write;

    #[test]
    fn test_generate_ssh_key() {
        // Create a temporary secret file with valid base64 content
        let mut secret_file = NamedTempFile::new().unwrap();
        let secret_content = base64::engine::general_purpose::STANDARD.encode(b"this_is_a_test_secret_key_32chars");
        secret_file.write_all(secret_content.as_bytes()).unwrap();
        let secret_file_path = secret_file.path().to_string_lossy().to_string();

        // Test key generation
        let result = generate_ssh_key(secret_file_path);

        match result {
            Ok((private_key, public_key)) => {
                // Verify the keys are not empty
                assert!(!private_key.is_empty(), "Private key should not be empty");
                assert!(!public_key.is_empty(), "Public key should not be empty");

                // Verify public key format (should start with "ssh-ed25519")
                assert!(public_key.starts_with("ssh-ed25519"), "Public key should start with 'ssh-ed25519'");

                // Verify private key is base64 encoded (encrypted)
                let _decoded = base64::engine::general_purpose::STANDARD.decode(&private_key)
                    .expect("Private key should be valid base64");

                println!("✓ SSH key generation test passed");
                println!("  Private key length: {} characters", private_key.len());
                println!("  Public key: {}", public_key);
            }
            Err(e) => {
                panic!("SSH key generation failed: {}", e);
            }
        }
    }
}

