/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use base64::{engine::general_purpose, Engine};
use ed25519_compact::KeyPair;
use entity::evaluation::EvaluationStatus;
use rand::rngs::OsRng;
use sea_orm::EntityTrait;
use ssh_key::{Algorithm, LineEnding, PrivateKey};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;
use tokio::process::Command;

use super::input::{check_repository_url_is_ssh, hex_to_vec, load_secret};
use super::types::*;

pub async fn check_project_updates(state: Arc<ServerState>, project: &MProject) -> (bool, Vec<u8>) {
    if state.cli.debug {
        println!(
            "Checking for updates on project: {} [{}]",
            project.id, project.name
        );
    };

    let cmd = match if check_repository_url_is_ssh(&project.repository) {
        let organization = EOrganization::find_by_id(project.organization)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();

        let (private_key, _public_key) =
            decrypt_ssh_private_key(load_secret(&state.cli.crypt_secret_file), organization)
                .unwrap();

        let ssh_key_path = write_key(private_key, state.cli.base_path.clone()).unwrap();

        let cmd = Command::new(state.cli.binpath_git.clone())
            .arg("ls-remote")
            .arg("-c")
            .arg(format!("core.sshCommand=ssh -i {}", ssh_key_path))
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
            println!("Error on executing command: {}", e);
            return (false, vec![]);
        }
    };

    let errmsg = String::from_utf8(cmd.stderr).unwrap();

    if !errmsg.is_empty() {
        println!("Error: {}", errmsg);
        return (false, vec![]);
    }

    let output = String::from_utf8(cmd.stdout).unwrap();
    let output = output.lines().collect::<Vec<&str>>()[0]
        .split_whitespace()
        .collect::<Vec<&str>>();

    if output.len() != 2 {
        println!("Error: no hash in git ls-remote output");
        return (false, vec![]);
    }

    let remote_hash = hex_to_vec(output[0]).unwrap();

    if project.force_evaluation {
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
            return (false, remote_hash);
        }

        let commit = ECommit::find_by_id(evaluation.commit)
            .one(&state.db)
            .await
            .unwrap()
            .unwrap();

        if commit.hash == remote_hash {
            return (false, remote_hash);
        }
    }

    (true, remote_hash)
}

pub fn write_key(private_key: String, to_path: String) -> Result<String, String> {
    let path = format!(
        "{}/loaded-credentials_{}.key",
        to_path,
        uuid::Uuid::new_v4()
    );

    fs::write(&path, private_key).map_err(|e| e.to_string())?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;

    Ok(path)
}

pub fn clear_key(path: String) -> Result<(), String> {
    fs::remove_file(&path).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn generate_ssh_key(secret_file: String) -> Result<(String, String), String> {
    let secret = general_purpose::STANDARD
        .decode(load_secret(&secret_file))
        .map_err(|_| "Failed to decode GRADIENT_CRYPT_SECRET".to_string())?;

    let mut csprng = OsRng;
    let private_key = PrivateKey::random(&mut csprng, Algorithm::Ed25519)
        .map_err(|e| "Failed to generate SSH-keypair: ".to_string() + &e.to_string())?;

    let private_key_openssh = private_key.to_openssh(LineEnding::LF).unwrap().to_string();

    let public_key_openssh = private_key
        .public_key()
        .to_openssh()
        .unwrap()
        .to_string()
        .split_whitespace()
        .collect::<Vec<&str>>()[1]
        .to_string();

    let public_key_openssh = format!("{} {}", Algorithm::Ed25519.as_str(), public_key_openssh);

    let encrypted_private_key = if let Some(p) = crypter::encrypt(secret, &private_key_openssh) {
        p
    } else {
        return Err("Failed to encrypt private key".to_string());
    };

    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok((encrypted_private_key, public_key_openssh))
}

pub fn decrypt_ssh_private_key(
    secret_file: String,
    organization: MOrganization,
) -> Result<(String, String), String> {
    let secret = general_purpose::STANDARD
        .decode(load_secret(&secret_file))
        .map_err(|_| "Failed to decode GRADIENT_CRYPT_SECRET".to_string())?;

    let encrypted_private_key = general_purpose::STANDARD
        .decode(organization.clone().private_key)
        .unwrap();

    let decrypted_private_key = if let Some(p) = crypter::decrypt(secret, encrypted_private_key) {
        p
    } else {
        return Err("Failed to decrypt private key".to_string());
    };

    let decrypted_private_key = String::from_utf8(decrypted_private_key).unwrap();
    let formatted_public_key = format_public_key(organization);

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
    let secret = general_purpose::STANDARD
        .decode(load_secret(&secret_file))
        .map_err(|_| "Failed to decode GRADIENT_CRYPT_SECRET".to_string())?;

    let encrypted_private_key = general_purpose::STANDARD
        .decode(cache.clone().signing_key)
        .unwrap();

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
) -> String {
    let secret = decrypt_signing_key(secret_file, cache.clone()).unwrap();
    let key = if public_key {
        secret.pk.as_ref()
    } else {
        secret.sk.as_ref()
    };

    let public_key = general_purpose::STANDARD.encode(key);
    let base_url = url
        .replace("https://", "")
        .replace("http://", "")
        .replace(":", "-");

    format!("{}-{}:{}", base_url, cache.name, public_key)
}

pub fn get_hash_from_url(url: String) -> Result<String, String> {
    let path_split = url.split('.').collect::<Vec<&str>>();

    if path_split.len() != 2
        && path_split[0].len() != 32
        && (path_split[1] != "narinfo" || path_split[1] != "nar")
        && !path_split[0]
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consts::NULL_TIME;

    #[test]
    fn test_check_generate_ssh_key() {
        let secret = general_purpose::STANDARD.encode("invalid");
        let (encrypted_private_key, public_key_openssh) = generate_ssh_key(secret.clone()).unwrap();

        let organization = MOrganization {
            id: uuid::Uuid::nil(),
            name: "test".to_string(),
            display_name: "test".to_string(),
            description: "test".to_string(),
            public_key: public_key_openssh,
            private_key: encrypted_private_key,
            use_nix_store: true,
            created_by: uuid::Uuid::nil(),
            created_at: *NULL_TIME,
        };

        let (_decrypted_private_key, _formatted_public_key) =
            decrypt_ssh_private_key(secret, organization.clone()).unwrap();

        println!("{}", _decrypted_private_key);
        println!("{}", format_public_key(organization.clone()));

        assert!(
            format_public_key(organization).starts_with("ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAI")
        );
    }
}
