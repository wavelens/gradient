/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
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
use tokio::process::Command;
use tracing::{debug, error, info, instrument};

use super::input::{check_repository_url_is_ssh, hex_to_vec, load_secret, vec_to_hex};
use super::types::*;

#[instrument(skip(state), fields(project_id = %project.id, project_name = %project.name))]
pub async fn check_project_updates(state: Arc<ServerState>, project: &MProject) -> (bool, Vec<u8>) {
    debug!("Checking for updates on project");

    let cmd = match if check_repository_url_is_ssh(&project.repository) {
        let organization = EOrganization::find_by_id(project.organization)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();

        let (private_key, _public_key) =
            decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization).unwrap();

        let ssh_key_path = write_key(private_key).unwrap();

        let cmd = Command::new(state.cli.binpath_git.clone())
            .arg("-c")
            .arg(format!("core.sshCommand={} -i {} -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null", state.cli.binpath_ssh, ssh_key_path))
            .arg("ls-remote")
            .arg(&project.repository)
            .output()
            .await;

        clear_key(ssh_key_path).unwrap();

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
            return (false, vec![]);
        }
    };

    if !cmd.status.success() {
        let errmsg = String::from_utf8(cmd.stderr).unwrap();
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
        return (false, vec![]);
    }

    let output = String::from_utf8(cmd.stdout).unwrap();
    let output = output.lines().collect::<Vec<&str>>()[0]
        .split_whitespace()
        .collect::<Vec<&str>>();

    if output.len() != 2 {
        error!("Invalid git ls-remote output format: expected hash and ref");
        return (false, vec![]);
    }

    let remote_hash = match hex_to_vec(output[0]) {
        Ok(hash) => hash,
        Err(e) => {
            error!(error = %e, "Failed to parse remote hash");
            return (false, vec![]);
        }
    };
    let remote_hash_str = output[0];
    debug!(remote_hash = %remote_hash_str, "Retrieved remote hash");

    if project.force_evaluation {
        info!("Force evaluation enabled, updating project");
        return (true, remote_hash);
    }

    if let Some(last_evaluation) = project.last_evaluation {
        let evaluation = EEvaluation::find_by_id(last_evaluation)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();

        if evaluation.status == EvaluationStatus::Queued
            || evaluation.status == EvaluationStatus::Evaluating
            || evaluation.status == EvaluationStatus::Building
        {
            debug!(status = ?evaluation.status, "Evaluation already in progress, skipping");
            return (false, remote_hash);
        }

        let commit = ECommit::find_by_id(evaluation.commit)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();

        if commit.hash == remote_hash {
            debug!("Remote hash matches current evaluation commit, no update needed");
            return (false, remote_hash);
        }

        info!("Remote hash differs from current evaluation commit, update needed");
    } else {
        info!("No previous evaluation found, update needed");
    }

    (true, remote_hash)
}

#[instrument(skip(state), fields(project_id = %project.id, project_name = %project.name, commit_hash = %vec_to_hex(commit_hash)))]
pub async fn get_commit_info(
    state: Arc<ServerState>,
    project: &MProject,
    commit_hash: &[u8],
) -> Result<(String, Option<String>, String), String> {
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
            .unwrap()
            .unwrap();

        let (private_key, _public_key) =
            decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization).unwrap();

        let ssh_key_path = write_key(private_key).unwrap();

        let cmd = Command::new(state.cli.binpath_git.clone())
            .arg("clone")
            .arg("--bare")
            .arg("-c")
            .arg(format!("core.sshCommand={} -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null", state.cli.binpath_ssh, ssh_key_path))
            .arg(&project.repository)
            .arg(&temp_dir)
            .output()
            .await;

        clear_key(ssh_key_path).unwrap();

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
                return Err(format!("Failed to clone repository: {}", err));
            }
        }
        Err(e) => {
            return Err(format!("Error executing git clone: {}", e));
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
                Err(format!("Git show failed: {}", err))
            } else {
                let stdout = String::from_utf8(output.stdout)
                    .map_err(|e| format!("Failed to parse stdout: {}", e))?;
                let lines: Vec<&str> = stdout.lines().collect();

                if lines.len() < 3 {
                    Err("Insufficient commit information returned from git".to_string())
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
        Err(e) => Err(format!("Error executing git show: {}", e)),
    };

    std::fs::remove_dir_all(&temp_dir).ok();

    result
}

pub fn write_key(private_key: String) -> Result<String, String> {
    let mut temp_file = NamedTempFile::with_suffix(".key").map_err(|e| e.to_string())?;

    fs::set_permissions(temp_file.path(), fs::Permissions::from_mode(0o600))
        .map_err(|e| e.to_string())?;

    temp_file
        .write_all(private_key.as_bytes())
        .map_err(|e| e.to_string())?;

    let path = temp_file.path().to_string_lossy().to_string();
    temp_file.keep().map_err(|e| e.to_string())?;

    Ok(path)
}

pub fn clear_key(path: String) -> Result<(), String> {
    fs::remove_file(&path).map_err(|e| e.to_string())?;
    Ok(())
}

#[instrument(skip(state, organization), fields(repository = %repository))]
pub async fn prefetch_flake(state: Arc<ServerState>, repository: String, organization: MOrganization) -> Result<(), String> {
    debug!("Prefetching flake inputs for repository: {}", repository);

    let (private_key, _public_key) =
        decrypt_ssh_private_key(state.cli.crypt_secret_file.clone(), organization).unwrap();

    let ssh_key_path = write_key(private_key).unwrap();

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
            return Err(format!("Failed to execute nix flake archive: {}", e));
        }
    };

    if !cmd.status.success() {
        let stderr = String::from_utf8_lossy(&cmd.stderr);
        error!(stderr = %stderr, "Nix flake archive command failed");

        if stderr.contains("command not found") || stderr.contains("No such file") {
            return Err("Nix is not installed or not found in PATH. Please ensure Nix is available.".to_string());
        } else if stderr.contains("Permission denied") || stderr.contains("authentication failed") {
            return Err("SSH authentication failed for flake input. Please check SSH key configuration.".to_string());
        } else if stderr.contains("Connection refused") || stderr.contains("Network is unreachable") {
            return Err("Network connection failed while fetching flake inputs.".to_string());
        } else {
            return Err(format!("Nix flake archive failed: {}", stderr));
        }
    }

    let stdout = String::from_utf8_lossy(&cmd.stdout);
    debug!(stdout = %stdout, "Nix flake archive completed successfully");

    Ok(())
}

pub fn generate_ssh_key(secret_file: String) -> Result<(String, String), String> {
    let secret = general_purpose::STANDARD
        .decode(load_secret(&secret_file))
        .map_err(|e| format!("Failed to decode GRADIENT_CRYPT_SECRET: {}", e))?;

    let keypair = KeyPair::generate();

    let public_key_bytes: [u8; 32] = keypair
        .pk
        .as_slice()
        .try_into()
        .map_err(|_| "Invalid public key length")?;
    let private_key_bytes: [u8; 32] = keypair
        .sk
        .as_slice()
        .try_into()
        .map_err(|_| "Invalid private key length")?;

    let keypair_data = KeypairData::Ed25519(Ed25519Keypair {
        public: Ed25519PublicKey::try_from(&public_key_bytes[..])
            .map_err(|e| format!("Failed to create public key: {}", e))?,
        private: Ed25519PrivateKey::from_bytes(&private_key_bytes),
    });
    let private_key = PrivateKey::new(keypair_data, "")
        .map_err(|e| format!("Failed to create SSH private key: {}", e))?;

    let private_key_openssh = private_key
        .to_openssh(LineEnding::LF)
        .map_err(|e| format!("Failed to convert private key to OpenSSH format: {}", e))?
        .to_string();

    let public_key_parts = private_key
        .public_key()
        .to_openssh()
        .map_err(|e| format!("Failed to convert public key to OpenSSH format: {}", e))?
        .to_string();

    let public_key_data = public_key_parts
        .split_whitespace()
        .nth(1)
        .ok_or("Invalid public key format")?;

    let public_key_openssh = format!("{} {}", Algorithm::Ed25519.as_str(), public_key_data);

    let encrypted_private_key =
        crypter::encrypt(secret, &private_key_openssh).ok_or("Failed to encrypt private key")?;

    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok((encrypted_private_key, public_key_openssh))
}

pub fn decrypt_ssh_private_key(
    secret_file: String,
    organization: MOrganization,
) -> Result<(String, String), String> {
    let secret_content = crate::input::load_secret(&secret_file);

    let secret = general_purpose::STANDARD
        .decode(&secret_content)
        .map_err(|e| format!("Failed to decode GRADIENT_CRYPT_SECRET from file '{}': {}. Please check that the file contains valid base64-encoded data.", secret_file, e))?;

    if secret.len() < 16 {
        return Err(format!(
            "GRADIENT_CRYPT_SECRET is too short ({} bytes). Encryption keys should be at least 16 bytes.",
            secret.len()
        ));
    }

    let encrypted_private_key = general_purpose::STANDARD
        .decode(organization.clone().private_key)
        .map_err(|e| format!("Failed to decode organization '{}' private key: {}. The private key in the database appears to be corrupted or not properly base64-encoded.", organization.name, e))?;

    let decrypted_private_key = if let Some(p) =
        crypter::decrypt(secret, encrypted_private_key.clone())
    {
        String::from_utf8(p)
            .map_err(|e| format!("Failed to convert decrypted private key to UTF-8: {}", e))?
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
                    return Err(format!(
                        "Failed to decrypt private key for organization '{}' and it doesn't appear to be plaintext. This usually indicates that the GRADIENT_CRYPT_SECRET from file '{}' does not match the key used to encrypt this organization's private key. Please verify the decryption key is correct.",
                        organization.name, secret_file
                    ));
                }
            }
            Err(_) => {
                return Err(format!(
                    "Failed to decrypt private key for organization '{}' and failed to decode as plaintext. This usually indicates that the GRADIENT_CRYPT_SECRET from file '{}' does not match the key used to encrypt this organization's private key. Please verify the decryption key is correct.",
                    organization.name, secret_file
                ));
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

pub fn generate_signing_key(secret_file: String) -> Result<String, String> {
    let secret = general_purpose::STANDARD
        .decode(load_secret(&secret_file))
        .map_err(|_| "Failed to decode GRADIENT_CRYPT_SECRET".to_string())?;

    let private_key = KeyPair::generate();
    let encrypted_private_key = if let Some(p) = crypter::encrypt(secret, *private_key) {
        p
    } else {
        return Err("Failed to encrypt private key".to_string());
    };

    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok(encrypted_private_key)
}

pub fn decrypt_signing_key(secret_file: String, cache: MCache) -> Result<KeyPair, String> {
    let secret_content = crate::input::load_secret(&secret_file);

    let secret = general_purpose::STANDARD
        .decode(&secret_content)
        .map_err(|e| format!("Failed to decode GRADIENT_CRYPT_SECRET from file '{}': {}. Please check that the file contains valid base64-encoded data.", secret_file, e))?;

    let encrypted_private_key = general_purpose::STANDARD
        .decode(cache.clone().signing_key)
        .map_err(|e| format!("Failed to decode cache '{}' signing key: {}. The signing key in the cache appears to be corrupted or not properly base64-encoded.", cache.name, e))?;

    let decrypted_private_key = if let Some(p) = crypter::decrypt(secret, encrypted_private_key) {
        p
    } else {
        return Err("Failed to decrypt private key".to_string());
    };

    let decrypted_private_key = KeyPair::from_slice(&decrypted_private_key)
        .map_err(|_| "Failed to convert decrypted private key to KeyPair".to_string())?;

    Ok(decrypted_private_key)
}

pub fn format_cache_key(
    secret_file: String,
    cache: MCache,
    url: String,
    public_key: bool,
) -> Result<String, String> {
    let secret = decrypt_signing_key(secret_file, cache.clone())?;
    let key: &[u8] = if public_key {
        secret.pk.as_ref()
    } else {
        secret.sk.as_ref()
    };

    let public_key = general_purpose::STANDARD.encode(key);
    let base_url = url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    Ok(format!("{}-{}:{}", base_url, cache.name, public_key))
}

pub fn get_hash_from_url(url: String) -> Result<String, String> {
    let path_split = url.split('.').collect::<Vec<&str>>();

    // Check if we have exactly 2 parts (hash.extension)
    if path_split.len() != 2 {
        return Err("Invalid path".to_string());
    }

    // Check hash length (32 characters)
    if path_split[0].len() != 32 {
        return Err("Invalid path".to_string());
    }

    // Check extension
    if path_split[1] != "narinfo" && path_split[1] != "nar" {
        return Err("Invalid path".to_string());
    }

    // Check hash characters (base32)
    if !path_split[0]
        .chars()
        .all(|c| "0123456789abcdfghijklmnpqrsvwxyz".contains(c))
    {
        return Err("Invalid path".to_string());
    }

    Ok(path_split[0].to_string())
}

pub fn get_hash_from_path(path: String) -> Result<(String, String), String> {
    let path_split = path.split('/').collect::<Vec<&str>>();
    if path_split.len() < 4 {
        return Err("Invalid path".to_string());
    }

    let path_split = path_split[3].split('-').collect::<Vec<&str>>();
    if path_split.len() < 2 {
        return Err("Invalid path".to_string());
    }

    let package = path_split[1..].join("-");
    let hash = path_split[0].to_string();

    Ok((hash, package))
}

pub fn get_path_from_build_output(build_output: MBuildOutput) -> String {
    format!("/nix/store/{}-{}", build_output.hash, build_output.package)
}

pub fn get_cache_nar_location(base_path: String, hash: String, compressed: bool) -> String {
    let hash_hex = hash.as_str();
    std::fs::create_dir_all(format!("{}/nars/{}", base_path, &hash_hex[0..2])).unwrap();
    format!(
        "{}/nars/{}/{}.nar{}",
        base_path,
        &hash_hex[0..2],
        &hash_hex[2..],
        if compressed { ".zst" } else { "" }
    )
}
