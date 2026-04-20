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
    let header = general_purpose::URL_SAFE_NO_PAD.encode(r#"{"alg":"RS256","typ":"JWT"}"#);
    let payload = general_purpose::URL_SAFE_NO_PAD
        .encode(format!(r#"{{"iat":{iat},"exp":{exp},"iss":"{app_id}"}}"#));
    let signing_input = format!("{header}.{payload}");

    // Parse DER-encoded RSA private key from PEM.
    let der = pem_to_der(private_key_pem).context("failed to decode GitHub App private key PEM")?;
    let key_pair =
        RsaKeyPair::from_pkcs8(&der).map_err(|e| anyhow!("invalid RSA private key: {e:?}"))?;

    let rng = ring::rand::SystemRandom::new();
    let mut signature = vec![0u8; key_pair.public().modulus_len()];
    key_pair
        .sign(
            &signature::RSA_PKCS1_SHA256,
            &rng,
            signing_input.as_bytes(),
            &mut signature,
        )
        .map_err(|e| anyhow!("RSA signing failed: {e:?}"))?;

    let sig_b64 = general_purpose::URL_SAFE_NO_PAD.encode(&signature);
    Ok(format!("{signing_input}.{sig_b64}"))
}

/// Strips PEM headers/footers and decodes the base64 body to DER bytes.
fn pem_to_der(pem: &str) -> Result<Vec<u8>> {
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
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

    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 2048-bit RSA private key in PKCS#8 PEM format, generated for tests only.
    const TEST_RSA_PEM: &str = "-----BEGIN PRIVATE KEY-----\n\
MIIEvgIBADANBgkqhkiG9w0BAQEFAASCBKgwggSkAgEAAoIBAQC8aqmZjqdQ+jUh\n\
BTRBZ2mAP72YnrDQyoaTqV3I4s2V+J5C6y1DsD622J3hBJl0szabLvD36EYi+XQ2\n\
1mAuH7O7mh5uO6slpcEtkkUO/51pMKLk58bkta57re5xZR7OZEjy6VX8l0mUNm5+\n\
1U+6WmfjhtlS9fzoDinEYXn2AVy90U577WrL0yf4nWea3TqMxPazbsV9FfrdowIr\n\
gfA2YB697vAAmaBw48VpJL+vKmjZ03CvTZHSIniniTN4IrdbSHWt6qRA4G7vIxn6\n\
3PMD5ddo0Ki0mEK9Y4Ik+OBfS2n/lB9XtZyFSK21tj/xrmZioSSrzqRo35xObNar\n\
iitWsCIDAgMBAAECggEABPfLAQGB5+CxBe3dMtxHK9qCZUPJ5bdxVPNiRam1Qf8M\n\
LTeGOgKrpKaRgx1b7nfLOMxEDkVRlBp/tfJxFsY/NvMZWk64dIWqUklJCNw0ilF9\n\
+gsic2VW9GvhnZvM9CQwyDbezYovpnjI8Q8uyvsXQiiMEmPqBzRYZZUsYhAnIdoX\n\
Wvh/AaexO7N370JEJTPOSuzY7OUBq7+vsNa7/6XE3uWxQYShdfoAQShFpBCkIkx9\n\
Z8X7ioxhI0o5NuNfMJ9UmWAq5Se2h+RmnbwTrQwZwqA0slKfCwAIO5CUt1XUJFWg\n\
OeIR7c0MELwmZ3ln7o+TxXgKxsuWR5PDX0nqU1O+AQKBgQDsH8Syr757YP1n6Q4u\n\
RE8K2It+c/ET6AxpPlJEtKLaRPGuFwcdCjWZ5tF8OHGZJZPLpTOQU1ckSuQF23Q2\n\
96EwFb1bxGSN4xMhMhEZx+Prs+zDUp5GDTDaXxlxXef4UrTeSwSoqt9pukzxkjOT\n\
rJC9BiGthjXq6P/DfbkBAdKcrwKBgQDMRtkE0/osFu2usqr89FHZFH8mxxSFqYkM\n\
/zKCQ9kK5HYfqLNB1GPD7hleFJsm+OWdS/hUYS0nX7PrE7S2UOjeYPPC9PyEqUH4\n\
d84CjKIY7qxK7x1XHV82pcCTwjJWGHZIO8IqZAtcA6z9PlKdYf0YrWgv8lANXzKQ\n\
W0GE0Gks7QKBgQC+uF5BUhCSSWIFN0pb9pK9mPD7P5zezlSQAWWj1x+fG4b2beUy\n\
AJgQ6k4UfubKo36AQ7yle5tsVg1d6ccxysxoMXcUk0oBDQPbkTwczcb8EAVSMv5i\n\
aK8oAx5i4k3G1s7+qitmLTZtiKwzhzqfsgfqlfRH25rbVj2X4om3FYjPQwKBgGsu\n\
GRvxZOfRN/Bbil+iiXdOy9A60Ee5RlFtbMDwfGa8rEW8LCG0IIxi1yiHw0hVe5Rm\n\
kesj+Z8ZFbuX4U9vcF+NmxiFliC89gI6SfsIctyGDhxbDZfxr01q9noQgHyv5Q/N\n\
WvkG+PbUbuWI16wAB930zh+qEdqSQmN/ngbjmuuZAoGBAODtrNOO+JK8Ie0egAKu\n\
nubGKVeJEtmvu7DWfBCp2agbtfMA4nI/t6qgIYGXC4Dy5r6n7IavwYHxWk5z7dyW\n\
emfsoO2V6PgIGpJCs2sHvsyBad7qTscRwPXlXAbHkKvYYryI6TAf2/DaQ+vEdiFK\n\
iw88K5/oFeMFr7syCSKTPeQD\n\
-----END PRIVATE KEY-----\n";

    // ── generate_jwt ─────────────────────────────────────────────────────────

    #[test]
    fn generate_jwt_three_parts() {
        let jwt = generate_jwt(12345, TEST_RSA_PEM).expect("generate_jwt failed");
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have header.payload.signature");
    }

    #[test]
    fn generate_jwt_header_rs256() {
        let jwt = generate_jwt(1, TEST_RSA_PEM).expect("generate_jwt failed");
        let header_b64 = jwt.split('.').next().unwrap();
        let header_json = general_purpose::URL_SAFE_NO_PAD.decode(header_b64).unwrap();
        let header_str = String::from_utf8(header_json).unwrap();
        assert!(
            header_str.contains("RS256"),
            "header must contain RS256: {header_str}"
        );
    }

    #[test]
    fn generate_jwt_payload_iss() {
        let app_id = 99887u64;
        let jwt = generate_jwt(app_id, TEST_RSA_PEM).expect("generate_jwt failed");
        let payload_b64 = jwt.split('.').nth(1).unwrap();
        let payload_json = general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .unwrap();
        let payload_str = String::from_utf8(payload_json).unwrap();
        assert!(
            payload_str.contains(&format!("\"iss\":\"{app_id}\"")),
            "payload must contain iss={app_id}: {payload_str}"
        );
    }

    #[test]
    fn generate_jwt_invalid_pem_err() {
        let result = generate_jwt(1, "not a pem");
        assert!(result.is_err(), "expected Err for invalid PEM");
    }

    fn compute_github_sig(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn compute_gitea_sig(secret: &str, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        hex::encode(mac.finalize().into_bytes())
    }

    // ── verify_github_signature ──────────────────────────────────────────────

    #[test]
    fn verify_github_signature_valid() {
        let body = b"hello world";
        let sig = compute_github_sig("my-secret", body);
        assert!(verify_github_signature("my-secret", &sig, body));
    }

    #[test]
    fn verify_github_signature_wrong_body() {
        let sig = compute_github_sig("my-secret", b"original");
        assert!(!verify_github_signature("my-secret", &sig, b"tampered"));
    }

    #[test]
    fn verify_github_signature_wrong_secret() {
        let sig = compute_github_sig("correct-secret", b"body");
        assert!(!verify_github_signature("wrong-secret", &sig, b"body"));
    }

    #[test]
    fn verify_github_signature_missing_prefix() {
        let sig = compute_gitea_sig("secret", b"body"); // bare hex, no sha256=
        assert!(!verify_github_signature("secret", &sig, b"body"));
    }

    #[test]
    fn verify_github_signature_invalid_hex() {
        assert!(!verify_github_signature("secret", "sha256=ZZZZ", b"body"));
    }

    #[test]
    fn verify_github_signature_empty_body() {
        let sig = compute_github_sig("secret", b"");
        assert!(verify_github_signature("secret", &sig, b""));
    }

    // ── verify_gitea_signature ───────────────────────────────────────────────

    #[test]
    fn verify_gitea_signature_valid() {
        let body = b"gitea payload";
        let sig = compute_gitea_sig("gitea-secret", body);
        assert!(verify_gitea_signature("gitea-secret", &sig, body));
    }

    #[test]
    fn verify_gitea_signature_whitespace_trimmed() {
        let body = b"payload";
        let sig = compute_gitea_sig("secret", body);
        let sig_with_space = format!("{sig}  ");
        assert!(verify_gitea_signature("secret", &sig_with_space, body));
    }

    #[test]
    fn verify_gitea_signature_wrong_secret() {
        let sig = compute_gitea_sig("correct", b"body");
        assert!(!verify_gitea_signature("wrong", &sig, b"body"));
    }

    #[test]
    fn verify_gitea_signature_invalid_hex() {
        assert!(!verify_gitea_signature("secret", "ZZZZ", b"body"));
    }

    // ── pem_to_der ───────────────────────────────────────────────────────────

    #[test]
    fn pem_to_der_valid() {
        let original = b"\x30\x82\x01\x22";
        let b64 = general_purpose::STANDARD.encode(original);
        let pem = format!("-----BEGIN TEST KEY-----\n{b64}\n-----END TEST KEY-----\n");
        let der = pem_to_der(&pem).unwrap();
        assert_eq!(der, original);
    }

    #[test]
    fn pem_to_der_invalid_base64() {
        let pem = "-----BEGIN TEST-----\nnot!valid!base64!!!\n-----END TEST-----\n";
        assert!(pem_to_der(pem).is_err());
    }

    #[test]
    fn pem_to_der_concatenates_multiple_lines() {
        // PEMs wrap their base64 body at 64 columns; the decoder must glue
        // every body line together before base64-decoding.
        let original = vec![0xAB; 96];
        let b64 = general_purpose::STANDARD.encode(&original);
        let (chunk1, rest) = b64.split_at(16);
        let (chunk2, chunk3) = rest.split_at(16);
        let pem = format!("-----BEGIN K-----\n{chunk1}\n{chunk2}\n{chunk3}\n-----END K-----\n");
        let der = pem_to_der(&pem).unwrap();
        assert_eq!(der, original);
    }

    #[test]
    fn generate_jwt_payload_iat_and_exp_diff_600() {
        // exp must be 10 minutes after iat (back-dated 60 s). Guards against
        // the constant drift mutations on iat/exp arithmetic.
        let jwt = generate_jwt(1, TEST_RSA_PEM).expect("generate_jwt failed");
        let payload_b64 = jwt.split('.').nth(1).unwrap();
        let payload_json = general_purpose::URL_SAFE_NO_PAD
            .decode(payload_b64)
            .unwrap();
        let payload: serde_json::Value = serde_json::from_slice(&payload_json).unwrap();
        let iat = payload["iat"].as_u64().expect("iat must be a u64");
        let exp = payload["exp"].as_u64().expect("exp must be a u64");
        assert_eq!(exp - iat, 660, "exp should be iat + 660 (60 back-date + 600 window)");
    }

    #[test]
    fn generate_jwt_signature_verifies_with_public_key() {
        // Round-trip: RS256-sign then verify with the corresponding public key
        // derived from the same PEM. Catches mutations that corrupt the
        // signing input (e.g. swapping header/payload or changing the
        // separator).
        use ring::signature::{RSA_PKCS1_2048_8192_SHA256, UnparsedPublicKey};

        let jwt = generate_jwt(42, TEST_RSA_PEM).expect("generate_jwt failed");
        let parts: Vec<&str> = jwt.split('.').collect();
        let signing_input = format!("{}.{}", parts[0], parts[1]);
        let sig = general_purpose::URL_SAFE_NO_PAD.decode(parts[2]).unwrap();

        let der = pem_to_der(TEST_RSA_PEM).unwrap();
        let key_pair = RsaKeyPair::from_pkcs8(&der).unwrap();
        // Extract public key components from the RsaKeyPair to build a verifier.
        let pub_key = key_pair.public().as_ref();
        let verifier = UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, pub_key);
        verifier
            .verify(signing_input.as_bytes(), &sig)
            .expect("signature must verify");
    }

    #[test]
    fn verify_github_signature_empty_header_rejected() {
        assert!(!verify_github_signature("secret", "", b"body"));
    }

    #[test]
    fn verify_gitea_signature_empty_header_rejected() {
        // Empty hex decodes to empty bytes — mac.verify_slice with empty
        // expected bytes must not accept any real signature.
        let body = b"body";
        assert!(!verify_gitea_signature("secret", "", body));
    }

    #[test]
    fn verify_github_signature_wrong_prefix_rejected() {
        // sha1= instead of sha256=
        let bare = compute_gitea_sig("secret", b"body");
        let sig = format!("sha1={bare}");
        assert!(!verify_github_signature("secret", &sig, b"body"));
    }
}
