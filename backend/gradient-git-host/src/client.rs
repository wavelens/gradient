/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_util::http::{
    HttpClientForUserSuppliedUrls, ResponseBodyReadWithLimitError, read_response_body_with_limit,
};
use gradient_util::http_base_url::ValidatedHttpBaseUrl;
use gradient_util::http_validation::{
    DestinationUrlRejectedError, find_allowlist_rejection_in_error_chain,
};
use gradient_util::outbound_http_status_error::OutboundHttpNonSuccessStatusError;
use reqwest::{Method, Response, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use std::fmt;

pub(crate) const GITHUB_API_URL: &str = "https://api.github.com";

pub(crate) fn validated_public_github_api_base_url() -> ValidatedHttpBaseUrl {
    GITHUB_API_URL
        .parse()
        .expect("The GitHub API URL constant is a valid base URL.")
}

#[cfg(test)]
pub(crate) fn http_client_allow_loopback() -> HttpClientForUserSuppliedUrls {
    let allowlist = gradient_util::http_validation::UserUrlDestinationAllowlist::parse(&[
        "127.0.0.0/8".to_owned(),
        "::1/128".to_owned(),
    ])
    .expect("loopback allow-list");
    HttpClientForUserSuppliedUrls::new(allowlist).expect("build test http client")
}

const GITHUB_HEADERS: &[(&str, &str)] = &[
    ("Accept", "application/vnd.github+json"),
    ("X-GitHub-Api-Version", "2022-11-28"),
];

const NO_BODY: Option<&()> = None;

#[derive(Clone, Copy)]
enum TokenHeader {
    AuthorizationToken,
    AuthorizationBearer,
    PrivateToken,
    NoAuthHeader,
}

impl TokenHeader {
    fn pair(self, token: &str) -> Option<(&'static str, String)> {
        match self {
            Self::AuthorizationToken => Some(("Authorization", format!("token {token}"))),
            Self::AuthorizationBearer => Some(("Authorization", format!("Bearer {token}"))),
            Self::PrivateToken => Some(("PRIVATE-TOKEN", token.to_owned())),
            Self::NoAuthHeader => None,
        }
    }
}

#[derive(Clone)]
pub(crate) struct HostClient {
    http: HttpClientForUserSuppliedUrls,
    base_url: ValidatedHttpBaseUrl,
    api_prefix: &'static str,
    token: String,
    token_header: TokenHeader,
    headers: &'static [(&'static str, &'static str)],
    user_agent: Option<&'static str>,
}

impl fmt::Debug for HostClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HostClient")
            .field("base_url", &self.base_url)
            .field("api_prefix", &self.api_prefix)
            .finish_non_exhaustive()
    }
}

impl HostClient {
    fn new(
        http: HttpClientForUserSuppliedUrls,
        base_url: ValidatedHttpBaseUrl,
        api_prefix: &'static str,
        token: impl Into<String>,
        token_header: TokenHeader,
        headers: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            http,
            base_url,
            api_prefix,
            token: token.into(),
            token_header,
            headers,
            user_agent: None,
        }
    }

    pub(crate) fn gitea(
        http: HttpClientForUserSuppliedUrls,
        base_url: ValidatedHttpBaseUrl,
        token: impl Into<String>,
    ) -> Self {
        Self::new(
            http,
            base_url,
            "/api/v1",
            token,
            TokenHeader::AuthorizationToken,
            &[],
        )
    }

    pub(crate) fn gitea_or_forgejo_oauth2(
        http: HttpClientForUserSuppliedUrls,
        base_url: ValidatedHttpBaseUrl,
    ) -> Self {
        Self::new(
            http,
            base_url,
            "/login/oauth",
            "",
            TokenHeader::NoAuthHeader,
            &[("Accept", "application/json")],
        )
    }

    pub(crate) fn gitlab(
        http: HttpClientForUserSuppliedUrls,
        base_url: ValidatedHttpBaseUrl,
        token: impl Into<String>,
    ) -> Self {
        Self::new(
            http,
            base_url,
            "/api/v4",
            token,
            TokenHeader::PrivateToken,
            &[],
        )
    }

    pub(crate) fn github(
        http: HttpClientForUserSuppliedUrls,
        base_url: ValidatedHttpBaseUrl,
        token: impl Into<String>,
    ) -> Self {
        Self::new(
            http,
            base_url,
            "",
            token,
            TokenHeader::AuthorizationBearer,
            GITHUB_HEADERS,
        )
    }

    pub(crate) fn with_user_agent(&self, user_agent: &'static str) -> Self {
        Self {
            user_agent: Some(user_agent),
            ..self.clone()
        }
    }

    pub(crate) fn base_url(&self) -> &str {
        self.base_url.as_str()
    }

    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    pub(crate) async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, HostError> {
        self.succeed(Method::GET, path, NO_BODY).await?.json().await
    }

    pub(crate) async fn get_json_optional<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<Option<T>, HostError> {
        let reply = self.call(Method::GET, path, NO_BODY).await?;
        if reply.status() == StatusCode::NOT_FOUND {
            return Ok(None);
        }

        reply.into_success().await?.json().await.map(Some)
    }

    pub(crate) async fn post_json<T: DeserializeOwned, B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<T, HostError> {
        self.succeed(Method::POST, path, Some(body))
            .await?
            .json()
            .await
    }

    pub(crate) async fn post_json_empty<T: DeserializeOwned>(
        &self,
        path: &str,
    ) -> Result<T, HostError> {
        self.succeed(Method::POST, path, NO_BODY)
            .await?
            .json()
            .await
    }

    pub(crate) async fn post<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), HostError> {
        self.succeed(Method::POST, path, Some(body)).await?;
        Ok(())
    }

    pub(crate) async fn patch<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), HostError> {
        self.succeed(Method::PATCH, path, Some(body)).await?;
        Ok(())
    }

    pub(crate) async fn delete(&self, path: &str) -> Result<(), HostError> {
        self.succeed(Method::DELETE, path, NO_BODY).await?;
        Ok(())
    }

    pub(crate) async fn put<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), HostError> {
        self.succeed(Method::PUT, path, Some(body)).await?;
        Ok(())
    }

    pub(crate) async fn post_form_json<T: DeserializeOwned, F: Serialize + ?Sized>(
        &self,
        path: &str,
        form: &F,
    ) -> Result<T, HostError> {
        let url = self.url(path);
        let request = self.request(Method::POST, &url)?.form(form);
        Self::send(Method::POST, url, request)
            .await?
            .into_success()
            .await?
            .json()
            .await
    }

    async fn succeed<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Reply, HostError> {
        self.call(method, path, body).await?.into_success().await
    }

    async fn call<B: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: Option<&B>,
    ) -> Result<Reply, HostError> {
        let url = self.url(path);
        let mut request = self.request(method.clone(), &url)?;
        if let Some(body) = body {
            request = request.json(body);
        }
        Self::send(method, url, request).await
    }

    fn url(&self, path: &str) -> String {
        self.base_url
            .append_api_path_to_base_url(&format!("{}{path}", self.api_prefix))
    }

    async fn send(
        method: Method,
        url: String,
        request: reqwest::RequestBuilder,
    ) -> Result<Reply, HostError> {
        match request.send().await {
            Ok(response) => Ok(Reply {
                method,
                url,
                response,
            }),
            Err(source) => Err(HostError {
                method,
                url,
                failure: Failure::Transport(source),
            }),
        }
    }

    fn request(&self, method: Method, url: &str) -> Result<reqwest::RequestBuilder, HostError> {
        let mut request = self
            .http
            .request_builder_for_checked_url(method.clone(), url)
            .map_err(|source| HostError {
                method,
                url: url.to_owned(),
                failure: Failure::Refused(source),
            })?;
        if let Some((name, value)) = self.token_header.pair(&self.token) {
            request = request.header(name, value);
        }
        for (name, value) in self.headers {
            request = request.header(*name, *value);
        }

        Ok(match self.user_agent {
            Some(user_agent) => request.header("User-Agent", user_agent),
            None => request,
        })
    }
}

struct Reply {
    method: Method,
    url: String,
    response: Response,
}

impl Reply {
    fn status(&self) -> StatusCode {
        self.response.status()
    }

    async fn into_success(self) -> Result<Self, HostError> {
        let status = self.status();
        if status.is_success() {
            return Ok(self);
        }

        let rejection =
            OutboundHttpNonSuccessStatusError::create_error_from_response_status_and_headers(
                status,
                self.response.headers(),
                status.to_string(),
            );
        if let Ok(body) = read_response_body_with_limit(
            self.response,
            MAX_LOGGED_GIT_HOST_ERROR_RESPONSE_BODY_BYTES,
        )
        .await
        {
            tracing::debug!(
                method = %self.method,
                url = %self.url,
                %status,
                body = %String::from_utf8_lossy(&body),
                "Git host responded with a non-success status"
            );
        }
        Err(HostError {
            method: self.method,
            url: self.url,
            failure: Failure::Status(rejection),
        })
    }

    async fn json<T: DeserializeOwned>(self) -> Result<T, HostError> {
        let failure = match read_response_body_with_limit(
            self.response,
            MAX_GIT_HOST_API_JSON_RESPONSE_BODY_BYTES,
        )
        .await
        {
            Ok(body) => match serde_json::from_slice(&body) {
                Ok(value) => return Ok(value),
                Err(source) => Failure::Decode(source),
            },
            Err(source) => Failure::Body(source),
        };
        Err(HostError {
            method: self.method,
            url: self.url,
            failure,
        })
    }
}

const MAX_LOGGED_GIT_HOST_ERROR_RESPONSE_BODY_BYTES: usize = 4 * 1024;
const MAX_GIT_HOST_API_JSON_RESPONSE_BODY_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug)]
enum Failure {
    Refused(DestinationUrlRejectedError),
    Transport(reqwest::Error),
    Status(OutboundHttpNonSuccessStatusError),
    Body(ResponseBodyReadWithLimitError),
    Decode(serde_json::Error),
}

#[derive(Debug)]
pub(crate) struct HostError {
    method: Method,
    url: String,
    failure: Failure,
}

impl HostError {
    pub(crate) fn status(&self) -> Option<StatusCode> {
        match self.failure {
            Failure::Status(ref rejection) => Some(rejection.status),
            _ => None,
        }
    }

    pub(crate) fn allowlist_refusal(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.failure {
            Failure::Refused(refusal) => Some(refusal),
            Failure::Transport(source) => find_allowlist_rejection_in_error_chain(source),
            _ => None,
        }
    }
}

impl fmt::Display for HostError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            method,
            url,
            failure,
        } = self;
        match failure {
            Failure::Refused(source) => write!(f, "{method} {url}: {source}"),
            Failure::Transport(source) => write!(f, "{method} {url}: {source}"),
            Failure::Status(_) => write!(f, "{method} {url}"),
            Failure::Body(source) => write!(f, "{method} {url}: Reading the Response: {source}"),
            Failure::Decode(source) => write!(f, "{method} {url}: decoding response: {source}"),
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.failure {
            Failure::Status(rejection) => Some(rejection),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};

    fn serve_once(response: &'static str) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept test connection");
            let mut request = [0u8; 4096];
            let _ = stream.read(&mut request);
            stream
                .write_all(response.as_bytes())
                .expect("write test response");
        });

        addr
    }

    #[tokio::test]
    async fn failing_status_reports_method_url_and_status_without_body() {
        let addr = serve_once(
            "HTTP/1.1 403 Forbidden\r\ncontent-length: 11\r\nconnection: close\r\n\r\nSECRET-BODY",
        );
        let client = HostClient::gitea(
            http_client_allow_loopback(),
            format!("http://{addr}").parse().expect("base url"),
            "tok",
        );

        let err = client
            .post(
                "/repos/acme/widgets/statuses/abc",
                &serde_json::json!({"state": "success"}),
            )
            .await
            .expect_err("a 403 must fail");

        assert_eq!(err.status(), Some(StatusCode::FORBIDDEN));
        assert_eq!(
            format!("{:#}", anyhow::Error::new(err)),
            format!("POST http://{addr}/api/v1/repos/acme/widgets/statuses/abc: 403 Forbidden")
        );
    }

    #[tokio::test]
    async fn loopback_base_url_is_refused_without_admin_allow_list() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind test listener");
        let addr = listener.local_addr().expect("listener address");
        let http = HttpClientForUserSuppliedUrls::new(Default::default()).expect("http client");
        let client = HostClient::gitea(
            http,
            format!("http://{addr}").parse().expect("base url"),
            "tok",
        );

        let err = client
            .get_json::<serde_json::Value>("/user")
            .await
            .expect_err("loopback target");

        assert!(matches!(err.failure, Failure::Refused(_)), "{err}");
        listener.set_nonblocking(true).expect("nonblocking");
        assert!(listener.accept().is_err(), "no connection was made");
    }
}
