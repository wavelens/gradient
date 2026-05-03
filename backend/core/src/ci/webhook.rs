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
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use tracing::{error, warn};
use uuid::Uuid;

type HmacSha256 = Hmac<Sha256>;

/// Validate a user-supplied webhook URL against SSRF-style abuse.
///
/// Rejects:
/// - schemes other than `http`/`https`,
/// - URLs without a host,
/// - IP literals in loopback / private / link-local / multicast /
///   unspecified / broadcast / shared (CGNAT) ranges,
/// - IPv6 literals in loopback / unspecified / multicast / unique-local
///   (`fc00::/7`) / link-local (`fe80::/10`) / IPv4-mapped unsafe ranges.
///
/// Hostnames are accepted at validation time; delivery-time DNS resolution
/// is re-checked in `ReqwestWebhookClient::deliver` to defend against DNS
/// pointing at internal addresses.
pub fn validate_webhook_url(url: &str) -> Result<reqwest::Url, String> {
    let parsed = reqwest::Url::parse(url).map_err(|e| format!("Invalid URL: {}", e))?;
    match parsed.scheme() {
        "http" | "https" => {}
        s => return Err(format!("URL scheme must be http or https, got '{}'", s)),
    }
    let host = parsed
        .host()
        .ok_or_else(|| "URL must include a host".to_string())?;
    match host {
        url::Host::Ipv4(ip) => {
            if is_unsafe_ipv4(&ip) {
                return Err(format!(
                    "URL points to a disallowed address ({}); private/loopback/link-local/cloud-metadata addresses are blocked",
                    ip
                ));
            }
        }
        url::Host::Ipv6(ip) => {
            if is_unsafe_ipv6(&ip) {
                return Err(format!(
                    "URL points to a disallowed address ({}); private/loopback/link-local addresses are blocked",
                    ip
                ));
            }
        }
        url::Host::Domain(d) => {
            if d.is_empty() {
                return Err("URL host is empty".to_string());
            }
            // Reject literal "localhost" — common typo bypass.
            if d.eq_ignore_ascii_case("localhost") {
                return Err("URL host 'localhost' is not allowed".to_string());
            }
        }
    }
    Ok(parsed)
}

/// Block IPv4 ranges that should never be reachable by an outbound webhook.
fn is_unsafe_ipv4(ip: &Ipv4Addr) -> bool {
    if ip.is_loopback()
        || ip.is_private()
        || ip.is_link_local()
        || ip.is_broadcast()
        || ip.is_multicast()
        || ip.is_unspecified()
    {
        return true;
    }
    let o = ip.octets();
    // Shared address space (CGNAT) 100.64.0.0/10.
    if o[0] == 100 && (o[1] & 0xC0) == 64 {
        return true;
    }
    // 0.0.0.0/8 — current network.
    if o[0] == 0 {
        return true;
    }
    // 192.0.0.0/24 — IETF protocol assignments.
    if o[0] == 192 && o[1] == 0 && o[2] == 0 {
        return true;
    }
    // Reserved 240.0.0.0/4 (excluding 255.255.255.255 already caught).
    if o[0] >= 240 {
        return true;
    }
    false
}

/// Block IPv6 ranges that should never be reachable by an outbound webhook.
fn is_unsafe_ipv6(ip: &Ipv6Addr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return true;
    }
    let segs = ip.segments();
    // Unique-local fc00::/7
    if (segs[0] & 0xfe00) == 0xfc00 {
        return true;
    }
    // Link-local fe80::/10
    if (segs[0] & 0xffc0) == 0xfe80 {
        return true;
    }
    // IPv4-mapped ::ffff:0:0/96 — check the embedded v4.
    if segs[0] == 0
        && segs[1] == 0
        && segs[2] == 0
        && segs[3] == 0
        && segs[4] == 0
        && segs[5] == 0xffff
    {
        let v4 = Ipv4Addr::new(
            (segs[6] >> 8) as u8,
            (segs[6] & 0xff) as u8,
            (segs[7] >> 8) as u8,
            (segs[7] & 0xff) as u8,
        );
        if is_unsafe_ipv4(&v4) {
            return true;
        }
    }
    false
}

fn is_unsafe_ip(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_unsafe_ipv4(v4),
        IpAddr::V6(v6) => is_unsafe_ipv6(v6),
    }
}

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
        Ok(Self::with_client(crate::http::build_client()?))
    }

    pub fn with_client(client: reqwest::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl WebhookClient for ReqwestWebhookClient {
    async fn deliver(&self, url: &str, signature: &str, event: &str, body: String) -> Result<u16> {
        let parsed = validate_webhook_url(url).map_err(|e| anyhow::anyhow!(e))?;

        // DNS-rebinding defence: resolve the host now and reject if any
        // resolved IP is in a disallowed range. We still hand the URL to
        // reqwest, which performs its own resolution — so this is a guard,
        // not a substitute for proper SSRF-aware connection wiring.
        if let Some(host) = parsed.host_str() {
            if matches!(parsed.host(), Some(url::Host::Domain(_))) {
                let port = parsed.port_or_known_default().unwrap_or(0);
                let lookup = tokio::net::lookup_host((host, port)).await?;
                for sa in lookup {
                    if is_unsafe_ip(&sa.ip()) {
                        anyhow::bail!(
                            "Webhook host '{}' resolved to a disallowed address ({})",
                            host,
                            sa.ip()
                        );
                    }
                }
            }
        }

        let resp = self
            .client
            .post(parsed)
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
        Some(project_id) => match EProject::find_by_id(project_id).one(&state.worker_db).await {
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
        .one(&state.worker_db)
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
    let evaluation = match EEvaluation::find_by_id(evaluation_id).one(&state.worker_db).await {
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

    match EProject::find_by_id(project_id).one(&state.worker_db).await {
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
        .all(&state.worker_db)
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
            &state.config.secrets.crypt_secret_file,
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

    #[test]
    fn decrypt_valid_base64_but_corrupt_ciphertext_fails() {
        // Base64-decodes fine but does not represent a valid `crypter` ciphertext.
        let (_f, path) = temp_secret_file();
        let garbage = general_purpose::STANDARD.encode(b"not-an-actual-ciphertext");
        let result = decrypt_webhook_secret(&path, &garbage);
        assert!(
            result.is_err(),
            "expected Err for well-formed base64 that is not a valid ciphertext"
        );
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
    fn sign_payload_different_body_different_sig() {
        // Guards against regressions that forget to hash the body.
        let a = sign_webhook_payload("secret", "body-one");
        let b = sign_webhook_payload("secret", "body-two");
        assert_ne!(a, b);
    }

    // ── validate_webhook_url ─────────────────────────────────────────────────

    #[test]
    fn validate_url_accepts_public_https() {
        assert!(validate_webhook_url("https://example.com/hook").is_ok());
        assert!(validate_webhook_url("http://example.com:8080/hook").is_ok());
        assert!(validate_webhook_url("https://example.com").is_ok());
    }

    #[test]
    fn validate_url_rejects_invalid_scheme() {
        assert!(validate_webhook_url("file:///etc/passwd").is_err());
        assert!(validate_webhook_url("ftp://example.com/").is_err());
        assert!(validate_webhook_url("gopher://example.com/").is_err());
        assert!(validate_webhook_url("javascript:alert(1)").is_err());
    }

    #[test]
    fn validate_url_rejects_unparseable() {
        assert!(validate_webhook_url("not a url").is_err());
        assert!(validate_webhook_url("").is_err());
    }

    #[test]
    fn validate_url_rejects_localhost_name() {
        assert!(validate_webhook_url("http://localhost/").is_err());
        assert!(validate_webhook_url("http://LOCALHOST/").is_err());
        assert!(validate_webhook_url("http://Localhost:8080/path").is_err());
    }

    #[test]
    fn validate_url_rejects_loopback_ipv4() {
        assert!(validate_webhook_url("http://127.0.0.1/").is_err());
        assert!(validate_webhook_url("http://127.255.255.254/").is_err());
    }

    #[test]
    fn validate_url_rejects_aws_metadata_ip() {
        // The motivating attack: cloud metadata service.
        assert!(validate_webhook_url("http://169.254.169.254/latest/meta-data/").is_err());
        // Generic link-local block.
        assert!(validate_webhook_url("http://169.254.0.1/").is_err());
    }

    #[test]
    fn validate_url_rejects_rfc1918_ranges() {
        assert!(validate_webhook_url("http://10.0.0.1/").is_err());
        assert!(validate_webhook_url("http://10.255.255.255/").is_err());
        assert!(validate_webhook_url("http://172.16.0.1/").is_err());
        assert!(validate_webhook_url("http://172.31.255.255/").is_err());
        assert!(validate_webhook_url("http://192.168.0.1/").is_err());
        assert!(validate_webhook_url("http://192.168.255.255/").is_err());
    }

    #[test]
    fn validate_url_rejects_cgnat_shared_space() {
        // 100.64.0.0/10 — RFC 6598.
        assert!(validate_webhook_url("http://100.64.0.1/").is_err());
        assert!(validate_webhook_url("http://100.127.255.254/").is_err());
        // Boundary: 100.128.0.0 is public.
        assert!(validate_webhook_url("http://100.128.0.1/").is_ok());
        assert!(validate_webhook_url("http://100.63.255.255/").is_ok());
    }

    #[test]
    fn validate_url_rejects_unspecified_and_broadcast() {
        assert!(validate_webhook_url("http://0.0.0.0/").is_err());
        assert!(validate_webhook_url("http://255.255.255.255/").is_err());
    }

    #[test]
    fn validate_url_rejects_multicast_ipv4() {
        assert!(validate_webhook_url("http://224.0.0.1/").is_err());
        assert!(validate_webhook_url("http://239.255.255.255/").is_err());
    }

    #[test]
    fn validate_url_rejects_reserved_ipv4() {
        // 240.0.0.0/4 reserved for future use.
        assert!(validate_webhook_url("http://240.0.0.1/").is_err());
    }

    #[test]
    fn validate_url_rejects_ipv6_loopback_and_unspecified() {
        assert!(validate_webhook_url("http://[::1]/").is_err());
        assert!(validate_webhook_url("http://[::]/").is_err());
    }

    #[test]
    fn validate_url_rejects_ipv6_link_and_unique_local() {
        assert!(validate_webhook_url("http://[fe80::1]/").is_err());
        assert!(validate_webhook_url("http://[febf::1]/").is_err());
        assert!(validate_webhook_url("http://[fc00::1]/").is_err());
        assert!(validate_webhook_url("http://[fdff::1]/").is_err());
    }

    #[test]
    fn validate_url_rejects_ipv6_multicast() {
        assert!(validate_webhook_url("http://[ff00::1]/").is_err());
        assert!(validate_webhook_url("http://[ff02::1]/").is_err());
    }

    #[test]
    fn validate_url_rejects_ipv4_mapped_loopback_in_ipv6() {
        // ::ffff:127.0.0.1
        assert!(validate_webhook_url("http://[::ffff:7f00:1]/").is_err());
        // ::ffff:169.254.169.254 — metadata IP via v4-mapped v6.
        assert!(validate_webhook_url("http://[::ffff:a9fe:a9fe]/").is_err());
    }

    #[test]
    fn validate_url_accepts_public_ipv4_literal() {
        // 8.8.8.8 is a public address — this is allowed.
        assert!(validate_webhook_url("http://8.8.8.8/").is_ok());
    }

    #[test]
    fn validate_url_accepts_public_ipv6_literal() {
        // 2001:4860:4860::8888 (Google public DNS) — allowed.
        assert!(validate_webhook_url("http://[2001:4860:4860::8888]/").is_ok());
    }

    #[test]
    fn sign_payload_roundtrip_with_verify_github() {
        let secret = "roundtrip-secret";
        let body = "some webhook payload";
        let sig = sign_webhook_payload(secret, body);
        assert!(verify_github_signature(secret, &sig, body.as_bytes()));
    }
}
