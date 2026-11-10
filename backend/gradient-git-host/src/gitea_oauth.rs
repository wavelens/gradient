/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use anyhow::{Context, Result, bail};
use gradient_types::GiteaOAuthConfig;
use serde::Deserialize;
use url::Url;

#[derive(Deserialize)]
pub struct GitHostOAuthTokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_in: Option<i64>,
}

pub fn build_git_host_oauth_authorization_url(
    app: &GiteaOAuthConfig,
    redirect_uri: &str,
    state: &str,
    pkce_challenge: &str,
) -> Result<Url> {
    let mut url = Url::parse(&format!("{}/login/oauth/authorize", app.instance_base_url))
        .context("Build the authorization URL for OAuth2")?;
    url.query_pairs_mut()
        .append_pair("client_id", &app.client_id)
        .append_pair("redirect_uri", redirect_uri)
        .append_pair("response_type", "code")
        .append_pair("state", state)
        .append_pair("code_challenge", pkce_challenge)
        .append_pair("code_challenge_method", "S256");
    Ok(url)
}

pub async fn exchange_authorization_code_for_tokens(
    http: &reqwest::Client,
    app: &GiteaOAuthConfig,
    code: &str,
    redirect_uri: &str,
    pkce_verifier: &str,
) -> Result<GitHostOAuthTokenResponse> {
    post_client_credentials_and_grant_fields_to_token_endpoint_and_parse_token_response(
        http,
        app,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("code_verifier", pkce_verifier),
        ],
    )
    .await
}

pub async fn get_new_tokens_from_git_host_with_refresh_token(
    http: &reqwest::Client,
    app: &GiteaOAuthConfig,
    refresh_token: &str,
) -> Result<GitHostOAuthTokenResponse> {
    post_client_credentials_and_grant_fields_to_token_endpoint_and_parse_token_response(
        http,
        app,
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
    .await
}

async fn post_client_credentials_and_grant_fields_to_token_endpoint_and_parse_token_response(
    http: &reqwest::Client,
    app: &GiteaOAuthConfig,
    grant_fields: &[(&str, &str)],
) -> Result<GitHostOAuthTokenResponse> {
    let client_secret = tokio::fs::read_to_string(&app.client_secret_file)
        .await
        .context("Read OAuth2 Client Secret File")?;
    let mut form = vec![
        ("client_id", app.client_id.as_str()),
        ("client_secret", client_secret.trim()),
    ];
    form.extend_from_slice(grant_fields);

    let resp = http
        .post(format!("{}/login/oauth/access_token", app.instance_base_url))
        .header(reqwest::header::ACCEPT, "application/json")
        .form(&form)
        .send()
        .await
        .context("OAuth2 Token Request to Git Host")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!("OAuth2 token endpoint for the Git host returned {status}: {body}");
    }
    resp.json::<GitHostOAuthTokenResponse>()
        .await
        .context("parse OAuth2 token response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_types::GitHostType;
    use wiremock::matchers::{body_string_contains, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn app(base: &str, secret_file: &tempfile::NamedTempFile) -> GiteaOAuthConfig {
        GiteaOAuthConfig {
            instance_base_url: base.to_owned(),
            git_host_type: GitHostType::Forgejo,
            client_id: "client".into(),
            client_secret_file: secret_file.path().to_string_lossy().into_owned(),
        }
    }

    fn secret_file() -> tempfile::NamedTempFile {
        let file = tempfile::NamedTempFile::new().expect("create secret file");
        std::fs::write(file.path(), "s3cret\n").expect("write secret file");
        file
    }

    #[test]
    fn authorization_url_carries_client_state_and_pkce_challenge() {
        let secret = secret_file();
        let url = build_git_host_oauth_authorization_url(
            &app("https://codeberg.org", &secret),
            "https://ci.example/api/v1/integrations/oauth/callback",
            "state-1",
            "challenge-1",
        )
        .expect("authorization url");
        assert_eq!(url.path(), "/login/oauth/authorize");
        let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(query["client_id"], "client");
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["state"], "state-1");
        assert_eq!(query["code_challenge"], "challenge-1");
        assert_eq!(query["code_challenge_method"], "S256");
    }

    #[tokio::test]
    async fn code_exchange_sends_client_credentials_and_verifier() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .and(body_string_contains("client_secret=s3cret"))
            .and(body_string_contains("grant_type=authorization_code"))
            .and(body_string_contains("code_verifier=verifier"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "access_token": "access",
                "refresh_token": "refresh",
                "expires_in": 3600,
                "token_type": "bearer",
            })))
            .expect(1)
            .mount(&server)
            .await;

        let secret = secret_file();
        let tokens = exchange_authorization_code_for_tokens(
            &reqwest::Client::new(),
            &app(&server.uri(), &secret),
            "code",
            "https://ci.example/callback",
            "verifier",
        )
        .await
        .expect("exchange code");
        assert_eq!(tokens.access_token, "access");
        assert_eq!(tokens.refresh_token.as_deref(), Some("refresh"));
        assert_eq!(tokens.expires_in, Some(3600));
    }

    #[tokio::test]
    async fn refresh_reports_rejected_grant() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/login/oauth/access_token"))
            .and(body_string_contains("grant_type=refresh_token"))
            .respond_with(
                ResponseTemplate::new(400)
                    .set_body_json(serde_json::json!({ "error": "invalid_grant" })),
            )
            .mount(&server)
            .await;

        let secret = secret_file();
        let err = get_new_tokens_from_git_host_with_refresh_token(
            &reqwest::Client::new(),
            &app(&server.uri(), &secret),
            "stale",
        )
        .await
        .err()
        .expect("rejected refresh");
        assert!(err.to_string().contains("invalid_grant"), "got: {err}");
    }
}
