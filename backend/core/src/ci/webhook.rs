/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::types::input::load_secret_bytes;
use crate::types::*;
use anyhow::Result;
use async_trait::async_trait;
use base64::{Engine, engine::general_purpose};
use entity::build::BuildStatus;
use entity::evaluation::EvaluationStatus;
use hmac::{Hmac, KeyInit, Mac};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use sha2::Sha256;
use std::sync::Arc;
use std::time::Duration;
use tracing::{error, warn};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// HTTP delivery for webhook payloads. Production impl uses `reqwest`; tests
/// can substitute an in-memory recorder.
#[async_trait]
pub trait WebhookClient: Send + Sync + std::fmt::Debug + 'static {
    /// POST `body` to `url` with the given signature/event headers.
    /// Returns the HTTP status code on success.
    async fn deliver(&self, url: &str, signature: &str, event: &str, body: String) -> Result<u16>;
}

/// Production `WebhookClient` backed by `reqwest`.
#[derive(Debug)]
pub struct ReqwestWebhookClient {
    client: reqwest::Client,
}

impl ReqwestWebhookClient {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { client })
    }
}

#[async_trait]
impl WebhookClient for ReqwestWebhookClient {
    async fn deliver(&self, url: &str, signature: &str, event: &str, body: String) -> Result<u16> {
        let resp = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .header("X-Gradient-Signature", signature)
            .header("X-Gradient-Event", event)
            .body(body)
            .send()
            .await?;
        Ok(resp.status().as_u16())
    }
}

/// Encrypts `plaintext_secret` with the server crypt key and returns a base64-encoded ciphertext.
pub fn encrypt_webhook_secret(crypt_secret_file: &str, plaintext: &str) -> Result<String, String> {
    let key = load_secret_bytes(crypt_secret_file);
    let ciphertext = crypter::encrypt_with_password(key.expose(), plaintext.as_bytes())
        .ok_or_else(|| "Encryption failed".to_string())?;
    Ok(general_purpose::STANDARD.encode(ciphertext))
}

/// Decrypts a base64-encoded ciphertext produced by `encrypt_webhook_secret`.
pub fn decrypt_webhook_secret(
    crypt_secret_file: &str,
    encoded: &str,
) -> Result<crate::types::SecretString, String> {
    let key = load_secret_bytes(crypt_secret_file);
    let ciphertext = general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| format!("Base64 decode error: {}", e))?;
    let plaintext = crypter::decrypt_with_password(key.expose(), ciphertext)
        .ok_or_else(|| "Decryption failed".to_string())?;
    String::from_utf8(plaintext)
        .map(crate::types::SecretString::new)
        .map_err(|e| format!("UTF-8 decode error: {}", e))
}

/// Signs `body` with HMAC-SHA256 using `secret` and returns `sha256=<hex>`.
pub fn sign_webhook_payload(secret: &str, body: &str) -> String {
    match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(mut mac) => {
            mac.update(body.as_bytes());
            format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
        }
        Err(_) => String::new(),
    }
}

pub async fn fire_evaluation_webhook(
    state: Arc<ServerState>,
    evaluation: MEvaluation,
    status: EvaluationStatus,
) {
    let event = match status {
        EvaluationStatus::Queued => "evaluation.queued",
        EvaluationStatus::Fetching => "evaluation.started",
        EvaluationStatus::EvaluatingFlake | EvaluationStatus::EvaluatingDerivation => {
            "evaluation.started"
        }
        EvaluationStatus::Building => "evaluation.building",
        EvaluationStatus::Waiting => "evaluation.waiting",
        EvaluationStatus::Completed => "evaluation.completed",
        EvaluationStatus::Failed => "evaluation.failed",
        EvaluationStatus::Aborted => "evaluation.aborted",
    };

    let org_id = match evaluation.project {
        Some(project_id) => match EProject::find_by_id(project_id).one(&state.db).await {
            Ok(Some(project)) => project.organization,
            Ok(None) => {
                warn!(evaluation_id = %evaluation.id, "Project not found for webhook delivery");
                return;
            }
            Err(e) => {
                error!(error = %e, evaluation_id = %evaluation.id, "DB error looking up project for webhook");
                return;
            }
        },
        None => return, // direct builds don't belong to an org project; skip
    };

    let payload = serde_json::json!({
        "evaluation_id": evaluation.id,
        "project_id": evaluation.project,
        "repository": evaluation.repository,
        "status": event,
    });

    fire_webhooks(state, org_id, event.to_string(), payload).await;
}

pub async fn fire_build_webhook(state: Arc<ServerState>, build: MBuild, status: BuildStatus) {
    let event = match status {
        BuildStatus::Queued => "build.queued",
        BuildStatus::Building => "build.started",
        BuildStatus::Completed => "build.completed",
        BuildStatus::Failed => "build.failed",
        BuildStatus::Substituted => "build.substituted",
        BuildStatus::Created | BuildStatus::Aborted | BuildStatus::DependencyFailed => return,
    };

    let org_id = match get_build_org_id(&state, build.evaluation).await {
        Some(id) => id,
        None => return,
    };

    // Look up the derivation path for the webhook payload (best-effort).
    let derivation_path = EDerivation::find_by_id(build.derivation)
        .one(&state.db)
        .await
        .ok()
        .flatten()
        .map(|d| d.derivation_path);

    let payload = serde_json::json!({
        "build_id": build.id,
        "evaluation_id": build.evaluation,
        "derivation_path": derivation_path,
        "status": event,
    });

    fire_webhooks(state, org_id, event.to_string(), payload).await;
}

async fn get_build_org_id(state: &Arc<ServerState>, evaluation_id: Uuid) -> Option<Uuid> {
    let evaluation = match EEvaluation::find_by_id(evaluation_id).one(&state.db).await {
        Ok(Some(e)) => e,
        Ok(None) => {
            warn!(evaluation_id = %evaluation_id, "Evaluation not found for webhook delivery");
            return None;
        }
        Err(e) => {
            error!(error = %e, evaluation_id = %evaluation_id, "DB error looking up evaluation for webhook");
            return None;
        }
    };

    let project_id = evaluation.project?;

    match EProject::find_by_id(project_id).one(&state.db).await {
        Ok(Some(project)) => Some(project.organization),
        Ok(None) => {
            warn!(project_id = %project_id, "Project not found for webhook delivery");
            None
        }
        Err(e) => {
            error!(error = %e, project_id = %project_id, "DB error looking up project for webhook");
            None
        }
    }
}

async fn fire_webhooks(
    state: Arc<ServerState>,
    org_id: Uuid,
    event: String,
    payload: serde_json::Value,
) {
    let webhooks = match EWebhook::find()
        .filter(CWebhook::Organization.eq(org_id))
        .filter(CWebhook::Active.eq(true))
        .all(&state.db)
        .await
    {
        Ok(w) => w,
        Err(e) => {
            error!(error = %e, org_id = %org_id, "Failed to query webhooks");
            return;
        }
    };

    if webhooks.is_empty() {
        return;
    }

    let body = serde_json::json!({
        "event": event,
        "data": payload,
    });

    let body_str = match serde_json::to_string(&body) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to serialize webhook payload");
            return;
        }
    };

    for webhook in webhooks {
        let subscribed = webhook
            .events
            .as_array()
            .map(|arr| arr.iter().any(|e| e.as_str() == Some(event.as_str())))
            .unwrap_or(false);

        if !subscribed {
            continue;
        }

        let plaintext_secret = match decrypt_webhook_secret(
            &state.cli.crypt_secret_file,
            &webhook.secret,
        ) {
            Ok(s) => s,
            Err(e) => {
                error!(error = %e, webhook_id = %webhook.id, "Failed to decrypt webhook secret; skipping delivery");
                continue;
            }
        };
        let signature = sign_webhook_payload(plaintext_secret.expose(), &body_str);

        let result = state
            .webhooks
            .deliver(&webhook.url, &signature, event.as_str(), body_str.clone())
            .await;

        match result {
            Ok(status) if !(200..300).contains(&status) => {
                warn!(
                    webhook_id = %webhook.id,
                    url = %webhook.url,
                    status = status,
                    "Webhook delivery returned non-success status"
                );
            }
            Err(e) => {
                warn!(error = %e, webhook_id = %webhook.id, url = %webhook.url, "Webhook delivery failed");
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ci::github_app::verify_github_signature;
    use std::io::Write;

    /// Write a temporary crypt secret file with a 32-char key and return the path.
    fn temp_secret_file() -> (tempfile::NamedTempFile, String) {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(b"test-secret-key-32-bytes-padding!").unwrap();
        f.flush().unwrap();
        let path = f.path().to_string_lossy().to_string();
        (f, path)
    }

    // ── encrypt_webhook_secret / decrypt_webhook_secret ───────────────────────

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let (_f, path) = temp_secret_file();
        let plaintext = "my-webhook-secret";
        let encrypted = encrypt_webhook_secret(&path, plaintext).expect("encrypt failed");
        let decrypted = decrypt_webhook_secret(&path, &encrypted).expect("decrypt failed");
        assert_eq!(decrypted.expose(), plaintext);
    }

    #[test]
    fn decrypt_invalid_base64_fails() {
        let (_f, path) = temp_secret_file();
        let result = decrypt_webhook_secret(&path, "!!!not-base64!!!");
        assert!(result.is_err(), "expected Err for invalid base64");
    }

    // ── sign_webhook_payload ─────────────────────────────────────────────────

    #[test]
    fn sign_payload_has_sha256_prefix() {
        let sig = sign_webhook_payload("secret", "body");
        assert!(
            sig.starts_with("sha256="),
            "expected sha256= prefix, got: {sig}"
        );
    }

    #[test]
    fn sign_payload_deterministic() {
        let a = sign_webhook_payload("secret", "body");
        let b = sign_webhook_payload("secret", "body");
        assert_eq!(a, b);
    }

    #[test]
    fn sign_payload_different_secret_different_sig() {
        let a = sign_webhook_payload("secret-a", "body");
        let b = sign_webhook_payload("secret-b", "body");
        assert_ne!(a, b);
    }

    #[test]
    fn sign_payload_roundtrip_with_verify_github() {
        let secret = "roundtrip-secret";
        let body = "some webhook payload";
        let sig = sign_webhook_payload(secret, body);
        assert!(verify_github_signature(secret, &sig, body.as_bytes()));
    }
}
