/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only OR WL-1.0
 */

use base64::{engine::general_purpose, Engine};
use entity::evaluation::EvaluationStatus;
use rand::rngs::OsRng;
use sea_orm::EntityTrait;
use ssh_key::{Algorithm, LineEnding, PrivateKey};
use std::sync::Arc;
use tokio::process::Command;

use super::input::hex_to_vec;
use super::types::*;

pub async fn check_project_updates(state: Arc<ServerState>, project: &MProject) -> (bool, Vec<u8>) {
    println!("Checking for updates on project: {}", project.id);

    let cmd = match Command::new(state.cli.binpath_git.clone())
        .arg("ls-remote")
        .arg(&project.repository)
        .output()
        .await
    {
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

pub fn generate_ssh_key(secret: String) -> Result<(String, String), String> {
    let mut csprng = OsRng;
    let private_key = PrivateKey::random(&mut csprng, Algorithm::Ed25519)
        .map_err(|e| "Failed to generate SSH key pair: ".to_string() + &e.to_string())?;

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

    let secret = general_purpose::STANDARD.decode(secret).unwrap();
    let encrypted_private_key = crypter::encrypt(secret, &private_key_openssh).unwrap();
    let encrypted_private_key = general_purpose::STANDARD.encode(&encrypted_private_key);

    Ok((encrypted_private_key, public_key_openssh))
}

pub fn decrypt_ssh_private_key(
    secret: String,
    organization: MOrganization,
) -> Result<(String, String), String> {
    let secret = general_purpose::STANDARD.decode(secret).unwrap();
    let encrypted_private_key = general_purpose::STANDARD
        .decode(organization.clone().private_key)
        .unwrap();
    let decrypted_private_key = crypter::decrypt(secret, encrypted_private_key).unwrap();
    let decrypted_private_key = String::from_utf8(decrypted_private_key).unwrap();
    let formatted_public_key = format_public_key(organization);

    Ok((decrypted_private_key, formatted_public_key))
}

pub fn format_public_key(organization: MOrganization) -> String {
    format!("{} {}", organization.public_key, organization.id)
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
            description: "test".to_string(),
            public_key: public_key_openssh,
            private_key: encrypted_private_key,
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
