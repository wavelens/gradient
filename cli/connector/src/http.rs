// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::ConnectorError;
use crate::auth::CliPollOutcome;
use futures::stream::{Stream, StreamExt};
use reqwest::{Method, RequestBuilder, Response};
use reqwest_streams::JsonStreamResponse;
use serde::de::DeserializeOwned;

#[derive(serde::Deserialize)]
struct Envelope<T> {
    error: bool,
    message: T,
}

#[derive(serde::Deserialize)]
struct ErrorEnvelope {
    #[serde(default)]
    code: Option<String>,
    message: String,
}

pub(crate) async fn decode<T: DeserializeOwned>(res: Response) -> Result<T, ConnectorError> {
    let status = res.status();
    let bytes = res.bytes().await?;

    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ConnectorError::Unauthorized);
    }

    if let Ok(env) = serde_json::from_slice::<Envelope<T>>(&bytes)
        && !env.error
    {
        return Ok(env.message);
    }

    if let Ok(env) = serde_json::from_slice::<Envelope<String>>(&bytes) {
        return Err(ConnectorError::Api {
            status,
            message: env.message,
        });
    }

    Err(ConnectorError::Api {
        status,
        message: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

pub(crate) async fn json_lines<T: DeserializeOwned + Send + 'static>(
    res: Response,
) -> Result<impl Stream<Item = Result<T, ConnectorError>> + use<T>, ConnectorError> {
    let status = res.status();
    if !status.is_success() {
        return Err(ConnectorError::Api {
            status,
            message: res.text().await?,
        });
    }
    Ok(res
        .json_nl_stream::<T>(1_024_000)
        .map(|r| r.map_err(|e| ConnectorError::Io(std::io::Error::other(e)))))
}

/// A `503` (upload budget full) or `429` (request rate limit) with `Retry-After` is resent after
/// that delay, at least one second since the rate limit rounds sub-second waits down to `0`.
pub(crate) async fn send_upload(
    build: impl Fn() -> Result<RequestBuilder, ConnectorError>,
) -> Result<Response, ConnectorError> {
    const ATTEMPTS: u32 = 20;
    let mut attempt = 1;
    loop {
        let resp = build()?.send().await?;
        let wait = matches!(
            resp.status(),
            reqwest::StatusCode::SERVICE_UNAVAILABLE | reqwest::StatusCode::TOO_MANY_REQUESTS
        )
        .then(|| resp.headers().get(reqwest::header::RETRY_AFTER))
        .flatten()
        .and_then(|v| v.to_str().ok()?.parse::<u64>().ok());
        match wait {
            Some(secs) if attempt < ATTEMPTS => {
                tokio::time::sleep(std::time::Duration::from_secs(secs.max(1))).await;
                attempt += 1;
            }
            _ => return Ok(resp),
        }
    }
}

pub(crate) fn build_url(base: &str, path: &str) -> String {
    format!(
        "{}/api/v1/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub(crate) async fn decode_cli_poll(res: Response) -> Result<CliPollOutcome, ConnectorError> {
    let status = res.status();
    let bytes = res.bytes().await?;

    if let Ok(env) = serde_json::from_slice::<Envelope<String>>(&bytes)
        && !env.error
    {
        return Ok(CliPollOutcome::Token(env.message));
    }

    if let Ok(env) = serde_json::from_slice::<ErrorEnvelope>(&bytes) {
        return match env.code.as_deref() {
            Some("cli_auth_pending") => Ok(CliPollOutcome::Pending),
            Some("cli_auth_expired") => Ok(CliPollOutcome::Expired),
            Some("cli_auth_denied") => Ok(CliPollOutcome::Denied),
            _ => Err(ConnectorError::Api {
                status,
                message: env.message,
            }),
        };
    }

    Err(ConnectorError::Api {
        status,
        message: String::from_utf8_lossy(&bytes).into_owned(),
    })
}

pub(crate) async fn decode_raw_string(res: Response) -> Result<String, ConnectorError> {
    let status = res.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ConnectorError::Unauthorized);
    }
    if !status.is_success() {
        return Err(ConnectorError::Api {
            status,
            message: res.text().await.unwrap_or_default(),
        });
    }
    Ok(res.text().await?)
}

pub(crate) fn request(
    http: &reqwest::Client,
    base_url: &str,
    token: Option<&str>,
    method: Method,
    endpoint: &str,
    auth_required: bool,
) -> Result<RequestBuilder, ConnectorError> {
    if auth_required && token.is_none() {
        return Err(ConnectorError::Unauthorized);
    }
    let mut rb = http.request(method, build_url(base_url, endpoint));
    if let Some(t) = token {
        rb = rb.header("Authorization", format!("Bearer {}", t));
    }
    Ok(rb)
}
