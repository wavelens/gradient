/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! GitHub App authentication and webhook verification.
//!
//! GitHub Apps authenticate as the app itself using a short-lived RS256 JWT,
//! then exchange it for a per-installation access token scoped to a specific
//! GitHub org/account. The access token can then be used as a Bearer token for
//! the GitHub REST API (e.g. to post commit statuses).

use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose};
use hmac::{Hmac, KeyInit, Mac};
use ring::signature::{self, RsaKeyPair};
use serde::Deserialize;
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

type HmacSha256 = Hmac<Sha256>;

// ── JWT generation ─────────────────────────────────────────────────────────

/// Generates a GitHub App JWT valid for up to 10 minutes.
///
/// The JWT is RS256-signed with the App's private key. GitHub requires:
/// - `iat`: issued-at (seconds since epoch, back-dated 60 s to account for clock skew)
/// - `exp`: expiry (≤ 10 minutes from now)
/// - `iss`: the numeric App ID as a string
pub fn generate_jwt(app_id: u64, private_key_pem: &str) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before epoch")?
        .as_secs();

    let iat = now - 60; // back-date 60 s for clock skew
    let exp = now + 600; // 10-minute window (GitHub max)

    // Encode header and payload as base64url (no padding).
    let header = general_purpose::URL_SAFE_NO_PAD
        .encode(r#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = general_purpose::URL_SAFE_NO_PAD
        .encode(format!(r#"{{"iat":{iat},"exp":{exp},"iss":"{app_id}"}}"#));
    let signing_input = format!("{header}.{payload}");

    // Parse DER-encoded RSA private key from PEM.
    let der = pem_to_der(private_key_pem).context("failed to decode GitHub App private key PEM")?;
    let key_pair = RsaKeyPair::from_pkcs8(&der)
        .map_err(|e| anyhow!("invalid RSA private key: {e:?}"))?;

    let rng = ring::rand::SystemRandom::new();
    let mut signature = vec![0u8; key_pair.public().modulus_len()];
    key_pair
        .sign(&signature::RSA_PKCS1_SHA256, &rng, signing_input.as_bytes(), &mut signature)
        .map_err(|e| anyhow!("RSA signing failed: {e:?}"))?;

    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(&signature);
    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Strips PEM headers/footers and decodes the base64 body to DER bytes.
fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let body: String = pem
        .lines()
        .filter(|l| !l.starts_with("-----"))
        .collect();
    general_purpose::STANDARD
        .decode(body.trim())
        .context("base64 decode of PEM body failed")
}

// ── Installation token exchange ────────────────────────────────────────────

#[derive(Deserialize)]
struct InstallationTokenResponse {
    token: String,
}

/// Fetches a short-lived installation access token for the given GitHub App
/// installation. The token is valid for ~1 hour and can be used as a Bearer
/// token against the GitHub REST API.
pub async fn get_installation_token(
    app_id: u64,
    private_key_pem: &str,
    installation_id: i64,
) -> Result<String> {
    let jwt = generate_jwt(app_id, private_key_pem)?;

    let client = reqwest::Client::builder()
        .user_agent("gradient-ci/1.0")
        .build()
        .context("failed to build reqwest client")?;

    let url = format!(
        "https://api.github.com/app/installations/{installation_id}/access_tokens"
    );
    debug!(installation_id, "requesting GitHub App installation token");

    let resp = client
        .post(&url)
        .bearer_auth(&jwt)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .context("GitHub installation token request failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("GitHub installation token API returned {status}: {body}");
    }

    let token_resp: InstallationTokenResponse = resp
        .json()
        .await
        .context("failed to parse GitHub installation token response")?;

    Ok(token_resp.token)
}

// ── Webhook signature verification ────────────────────────────────────────

/// Verifies a GitHub webhook signature from the `X-Hub-Signature-256` header.
///
/// The header value is expected to be `sha256=<hex>`. Returns `true` when the
/// computed HMAC-SHA256 of `body` with `secret` matches the provided signature.
pub fn verify_github_signature(secret: &str, signature_header: &str, body: &[u8]) -> bool {
    let expected_hex = match signature_header.strip_prefix("sha256=") {
        Some(h) => h,
        None => return false,
    };

    let Ok(expected_bytes) = hex::decode(expected_hex) else {
        return false;
    };

    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected_bytes).is_ok()
}

/// Verifies a Gitea/Forgejo webhook signature from the `X-Gitea-Signature` header.
///
/// The header value is a bare hex-encoded HMAC-SHA256 digest (no prefix).
pub fn verify_gitea_signature(secret: &str, signature_header: &str, body: &[u8]) -> bool {
    let Ok(expected_bytes) = hex::decode(signature_header.trim()) else {
        return false;
    };

    let Ok(mut mac) = HmacSha256::new_from_slice(secret.as_bytes()) else {
        return false;
    };
    mac.update(body);
    mac.verify_slice(&expected_bytes).is_ok()
}
