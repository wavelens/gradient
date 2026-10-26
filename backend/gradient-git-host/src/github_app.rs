/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::client::{GITHUB_API_URL, HostClient};
use anyhow::{Context, Result};
use hmac::{Hmac, KeyInit, Mac};
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::debug;

type HmacSha256 = Hmac<Sha256>;

#[derive(Serialize, Deserialize)]
struct AppClaims {
    iat: u64,
    exp: u64,
    iss: String,
}

/// GitHub App manifests return PKCS#1 keys; PKCS#8 keys are accepted as well.
pub fn generate_jwt(app_id: u64, private_key_pem: &str) -> Result<String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system time before epoch")?
        .as_secs();
    let claims = AppClaims {
        iat: now - 60,
        exp: now + 600,
        iss: app_id.to_string(),
    };
    let key = EncodingKey::from_rsa_pem(private_key_pem.as_bytes())
        .context("parse GitHub App private key PEM")?;
    encode(&Header::new(Algorithm::RS256), &claims, &key).context("sign GitHub App JWT")
}

#[derive(Deserialize)]
struct InstallationTokenResponse {
    token: String,
}

pub async fn get_installation_token(
    client: &reqwest::Client,
    app_id: u64,
    private_key_pem: &str,
    installation_id: i64,
) -> Result<String> {
    let jwt = generate_jwt(app_id, private_key_pem)?;

    debug!(installation_id, "requesting GitHub App installation token");

    let token_resp: InstallationTokenResponse =
        HostClient::github(client.clone(), GITHUB_API_URL, jwt)
            .post_json_empty(&format!(
                "/app/installations/{installation_id}/access_tokens"
            ))
            .await?;

    Ok(token_resp.token)
}

#[derive(Deserialize)]
struct InstallationResponse {
    account: InstallationAccount,
}

#[derive(Deserialize)]
struct InstallationAccount {
    login: String,
}

#[derive(Deserialize)]
struct AppResponse {
    html_url: String,
}

pub async fn get_installation(
    client: &reqwest::Client,
    app_id: u64,
    private_key_pem: &str,
    installation_id: i64,
) -> Result<String> {
    let installation: InstallationResponse = app_get(
        client,
        app_id,
        private_key_pem,
        &format!("app/installations/{installation_id}"),
    )
    .await?;
    Ok(installation.account.login)
}

pub async fn get_install_url(
    client: &reqwest::Client,
    app_id: u64,
    private_key_pem: &str,
) -> Result<String> {
    let app: AppResponse = app_get(client, app_id, private_key_pem, "app").await?;
    Ok(format!(
        "{}/installations/new",
        app.html_url.trim_end_matches('/')
    ))
}

async fn app_get<T: serde::de::DeserializeOwned>(
    client: &reqwest::Client,
    app_id: u64,
    private_key_pem: &str,
    path: &str,
) -> Result<T> {
    let jwt = generate_jwt(app_id, private_key_pem)?;

    Ok(HostClient::github(client.clone(), GITHUB_API_URL, jwt)
        .with_user_agent("gradient")
        .get_json(&format!("/{path}"))
        .await?)
}

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

    const TEST_RSA_PEM_PKCS1: &str = "-----BEGIN RSA PRIVATE KEY-----\n\
MIIEpAIBAAKCAQEAvGqpmY6nUPo1IQU0QWdpgD+9mJ6w0MqGk6ldyOLNlfieQust\n\
Q7A+ttid4QSZdLM2my7w9+hGIvl0NtZgLh+zu5oebjurJaXBLZJFDv+daTCi5OfG\n\
5LWue63ucWUezmRI8ulV/JdJlDZuftVPulpn44bZUvX86A4pxGF59gFcvdFOe+1q\n\
y9Mn+J1nmt06jMT2s27FfRX63aMCK4HwNmAeve7wAJmgcOPFaSS/rypo2dNwr02R\n\
0iJ4p4kzeCK3W0h1reqkQOBu7yMZ+tzzA+XXaNCotJhCvWOCJPjgX0tp/5QfV7Wc\n\
hUittbY/8a5mYqEkq86kaN+cTmzWq4orVrAiAwIDAQABAoIBAAT3ywEBgefgsQXt\n\
3TLcRyvagmVDyeW3cVTzYkWptUH/DC03hjoCq6SmkYMdW+53yzjMRA5FUZQaf7Xy\n\
cRbGPzbzGVpOuHSFqlJJSQjcNIpRffoLInNlVvRr4Z2bzPQkMMg23s2KL6Z4yPEP\n\
Lsr7F0IojBJj6gc0WGWVLGIQJyHaF1r4fwGnsTuzd+9CRCUzzkrs2OzlAau/r7DW\n\
u/+lxN7lsUGEoXX6AEEoRaQQpCJMfWfF+4qMYSNKOTbjXzCfVJlgKuUntofkZp28\n\
E60MGcKgNLJSnwsACDuQlLdV1CRVoDniEe3NDBC8Jmd5Z+6Pk8V4CsbLlkeTw19J\n\
6lNTvgECgYEA7B/Esq++e2D9Z+kOLkRPCtiLfnPxE+gMaT5SRLSi2kTxrhcHHQo1\n\
mebRfDhxmSWTy6UzkFNXJErkBdt0NvehMBW9W8RkjeMTITIRGcfj67Psw1KeRg0w\n\
2l8ZcV3n+FK03ksEqKrfabpM8ZIzk6yQvQYhrYY16uj/w325AQHSnK8CgYEAzEbZ\n\
BNP6LBbtrrKq/PRR2RR/JscUhamJDP8ygkPZCuR2H6izQdRjw+4ZXhSbJvjlnUv4\n\
VGEtJ1+z6xO0tlDo3mDzwvT8hKlB+HfOAoyiGO6sSu8dVx1fNqXAk8IyVhh2SDvC\n\
KmQLXAOs/T5SnWH9GK1oL/JQDV8ykFtBhNBpLO0CgYEAvrheQVIQkkliBTdKW/aS\n\
vZjw+z+c3s5UkAFlo9cfnxuG9m3lMgCYEOpOFH7myqN+gEO8pXubbFYNXenHMcrM\n\
aDF3FJNKAQ0D25E8HM3G/BAFUjL+YmivKAMeYuJNxtbO/qorZi02bYisM4c6n7IH\n\
6pX0R9ua21Y9l+KJtxWIz0MCgYBrLhkb8WTn0TfwW4pfool3TsvQOtBHuUZRbWzA\n\
8HxmvKxFvCwhtCCMYtcoh8NIVXuUZpHrI/mfGRW7l+FPb3BfjZsYhZYgvPYCOkn7\n\
CHLchg4cWw2X8a9NavZ6EIB8r+UPzVr5Bvj21G7liNesAAfd9M4fqhHakkJjf54G\n\
45rrmQKBgQDg7azTjviSvCHtHoACrp7mxilXiRLZr7uw1nwQqdmoG7XzAOJyP7eq\n\
oCGBlwuA8ua+p+yGr8GB8VpOc+3clnpn7KDtlej4CBqSQrNrB77MgWne6k7HEcD1\n\
5VwGx5Cr2GK8iOkwH9vw2kPrxHYhSosPPCuf6BXjBa+7Mgkikz3kAw==\n\
-----END RSA PRIVATE KEY-----\n";

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

    #[test]
    fn generate_jwt_rejects_invalid_pem() {
        assert!(
            generate_jwt(1, "not a pem").is_err(),
            "expected Err for invalid PEM"
        );
    }

    const TEST_RSA_PUBLIC_PEM: &str = "-----BEGIN PUBLIC KEY-----\n\
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAvGqpmY6nUPo1IQU0QWdp\n\
gD+9mJ6w0MqGk6ldyOLNlfieQustQ7A+ttid4QSZdLM2my7w9+hGIvl0NtZgLh+z\n\
u5oebjurJaXBLZJFDv+daTCi5OfG5LWue63ucWUezmRI8ulV/JdJlDZuftVPulpn\n\
44bZUvX86A4pxGF59gFcvdFOe+1qy9Mn+J1nmt06jMT2s27FfRX63aMCK4HwNmAe\n\
ve7wAJmgcOPFaSS/rypo2dNwr02R0iJ4p4kzeCK3W0h1reqkQOBu7yMZ+tzzA+XX\n\
aNCotJhCvWOCJPjgX0tp/5QfV7WchUittbY/8a5mYqEkq86kaN+cTmzWq4orVrAi\n\
AwIDAQAB\n\
-----END PUBLIC KEY-----\n";

    #[test]
    fn generate_jwt_signs_pkcs1_and_pkcs8_keys_that_verify() {
        use jsonwebtoken::{DecodingKey, Validation, decode};

        let key = DecodingKey::from_rsa_pem(TEST_RSA_PUBLIC_PEM.as_bytes()).unwrap();
        let validation = Validation::new(Algorithm::RS256);
        for (app_id, pem) in [(7, TEST_RSA_PEM_PKCS1), (42, TEST_RSA_PEM)] {
            let jwt = generate_jwt(app_id, pem).expect("key must be accepted");
            let claims = decode::<AppClaims>(&jwt, &key, &validation)
                .expect("signature must verify")
                .claims;
            assert_eq!(claims.iss, app_id.to_string());
            assert_eq!(claims.exp - claims.iat, 660);
        }
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

    #[test]
    fn verify_github_signature_accepts_only_its_own_hmac() {
        let body = b"hello world";
        let sig = compute_github_sig("my-secret", body);
        assert!(verify_github_signature("my-secret", &sig, body));
        let empty = compute_github_sig("secret", b"");
        assert!(verify_github_signature("secret", &empty, b""));
    }

    #[test]
    fn verify_github_signature_rejects_mismatch_or_malformed_header() {
        let body = b"body";
        let wrong_body = compute_github_sig("my-secret", b"original");
        let wrong_secret = compute_github_sig("correct-secret", body);
        let bare = compute_gitea_sig("secret", body);
        let wrong_prefix = format!("sha1={bare}");
        let cases: [(&str, &str, &[u8]); 6] = [
            ("my-secret", &wrong_body, b"tampered"),
            ("wrong-secret", &wrong_secret, body),
            ("secret", &bare, body),
            ("secret", "sha256=ZZZZ", body),
            ("secret", "", body),
            ("secret", &wrong_prefix, body),
        ];
        for (secret, sig, payload) in cases {
            assert!(!verify_github_signature(secret, sig, payload), "{sig}");
        }
    }

    #[test]
    fn verify_gitea_signature_accepts_its_hmac_and_trims_whitespace() {
        let body = b"payload";
        let sig = compute_gitea_sig("secret", body);
        assert!(verify_gitea_signature("secret", &sig, body));
        assert!(verify_gitea_signature("secret", &format!("{sig}  "), body));
    }

    #[test]
    fn verify_gitea_signature_rejects_mismatch_or_malformed_header() {
        let body = b"body";
        let wrong_secret = compute_gitea_sig("correct", body);
        for (secret, sig) in [
            ("wrong", wrong_secret.as_str()),
            ("secret", "ZZZZ"),
            ("secret", ""),
        ] {
            assert!(!verify_gitea_signature(secret, sig, body), "{sig}");
        }
    }
}
