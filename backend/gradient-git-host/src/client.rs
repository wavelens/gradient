/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use reqwest::{Method, Response, StatusCode};
use serde::{Serialize, de::DeserializeOwned};
use std::fmt;

pub(crate) const GITHUB_API_URL: &str = "https://api.github.com";

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
}

impl TokenHeader {
    fn pair(self, token: &str) -> (&'static str, String) {
        match self {
            Self::AuthorizationToken => ("Authorization", format!("token {token}")),
            Self::AuthorizationBearer => ("Authorization", format!("Bearer {token}")),
            Self::PrivateToken => ("PRIVATE-TOKEN", token.to_owned()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct HostClient {
    http: reqwest::Client,
    base_url: String,
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
        http: reqwest::Client,
        base_url: impl Into<String>,
        api_prefix: &'static str,
        token: impl Into<String>,
        token_header: TokenHeader,
        headers: &'static [(&'static str, &'static str)],
    ) -> Self {
        Self {
            http,
            base_url: base_url.into(),
            api_prefix,
            token: token.into(),
            token_header,
            headers,
            user_agent: None,
        }
    }

    pub(crate) fn gitea(
        http: reqwest::Client,
        base_url: impl Into<String>,
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

    pub(crate) fn gitlab(
        http: reqwest::Client,
        base_url: impl Into<String>,
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
        http: reqwest::Client,
        base_url: impl Into<String>,
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
        &self.base_url
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

    pub(crate) async fn put<B: Serialize + ?Sized>(
        &self,
        path: &str,
        body: &B,
    ) -> Result<(), HostError> {
        self.succeed(Method::PUT, path, Some(body)).await?;
        Ok(())
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
        let url = format!("{}{}{}", self.base_url, self.api_prefix, path);
        let mut request = self.request(method.clone(), &url);
        if let Some(body) = body {
            request = request.json(body);
        }

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

    fn request(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
        let (name, value) = self.token_header.pair(&self.token);
        let mut request = self.http.request(method, url).header(name, value);
        for (name, value) in self.headers {
            request = request.header(*name, *value);
        }

        match self.user_agent {
            Some(user_agent) => request.header("User-Agent", user_agent),
            None => request,
        }
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

        let body = self.response.text().await.unwrap_or_default();
        Err(HostError {
            method: self.method,
            url: self.url,
            failure: Failure::Status { status, body },
        })
    }

    async fn json<T: DeserializeOwned>(self) -> Result<T, HostError> {
        match self.response.json().await {
            Ok(value) => Ok(value),
            Err(source) => Err(HostError {
                method: self.method,
                url: self.url,
                failure: Failure::Decode(source),
            }),
        }
    }
}

#[derive(Debug)]
enum Failure {
    Transport(reqwest::Error),
    Status { status: StatusCode, body: String },
    Decode(reqwest::Error),
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
            Failure::Status { status, .. } => Some(status),
            Failure::Transport(_) | Failure::Decode(_) => None,
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
            Failure::Transport(source) => write!(f, "{method} {url}: {source}"),
            Failure::Status { status, body } => {
                write!(f, "{method} {url} returned {status}: {body}")
            }
            Failure::Decode(source) => write!(f, "{method} {url}: decoding response: {source}"),
        }
    }
}

impl std::error::Error for HostError {}

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
    async fn failing_status_reports_method_url_status_and_body() {
        let addr = serve_once(
            "HTTP/1.1 403 Forbidden\r\ncontent-length: 9\r\nconnection: close\r\n\r\nno access",
        );
        let http = gradient_util::http::build_client().expect("build test http client");
        let client = HostClient::gitea(http, format!("http://{addr}"), "tok");

        let err = client
            .post(
                "/repos/acme/widgets/statuses/abc",
                &serde_json::json!({"state": "success"}),
            )
            .await
            .expect_err("a 403 must fail");

        assert_eq!(err.status(), Some(StatusCode::FORBIDDEN));
        assert_eq!(
            err.to_string(),
            format!(
                "POST http://{addr}/api/v1/repos/acme/widgets/statuses/abc returned 403 Forbidden: no access"
            )
        );
    }
}
