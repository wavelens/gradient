// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::ErrorResponseBody;
use crate::auth::CliPollOutcome;
use crate::{Client, ConnectorError};
use bytes::Bytes;
use futures::stream::{Stream, StreamExt};
use reqwest::{Method, RequestBuilder, Response};
use reqwest_streams::JsonStreamResponse;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Message, http::HeaderValue};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

#[derive(Debug, Clone)]
pub struct DownloadedBinaryFile {
    pub suggested_filename: Option<String>,
    pub bytes: bytes::Bytes,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveUpdateEvent {
    pub event: String,
    pub at: String,
    pub content: serde_json::Value,
}

#[derive(serde::Deserialize)]
struct Envelope<T> {
    error: bool,
    message: T,
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
    timer: &TransferNoProgressTimer,
    build: impl Fn() -> Result<RequestBuilder, ConnectorError>,
) -> Result<Response, ConnectorError> {
    const ATTEMPTS: u32 = 20;
    let mut attempt = 1;
    loop {
        let resp = timer
            .send_request_and_timeout_when_no_progress_for_idle_limit(build()?)
            .await?;
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

#[derive(Clone)]
pub(crate) struct TransferNoProgressTimer {
    last_progress: Arc<Mutex<Instant>>,
    idle_limit: Duration,
}

impl TransferNoProgressTimer {
    const BODY_PIECE_SIZE: usize = 256 * 1024;

    pub(crate) fn new(idle_limit: Duration) -> Self {
        Self {
            last_progress: Arc::new(Mutex::new(Instant::now())),
            idle_limit,
        }
    }

    fn record_progress(&self) {
        *self
            .last_progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Instant::now();
    }

    fn idle_deadline(&self) -> Instant {
        *self
            .last_progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            + self.idle_limit
    }

    fn stalled_error(&self) -> ConnectorError {
        ConnectorError::Io(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!(
                "Transfer stalled: no data moved for {} seconds",
                self.idle_limit.as_secs()
            ),
        ))
    }

    pub(crate) fn request_body_stream_from_bytes_recording_progress(
        &self,
        bytes: Bytes,
    ) -> reqwest::Body {
        let timer = self.clone();
        let pieces = (0..bytes.len())
            .step_by(Self::BODY_PIECE_SIZE)
            .map(move |start| bytes.slice(start..(start + Self::BODY_PIECE_SIZE).min(bytes.len())));
        reqwest::Body::wrap_stream(futures::stream::iter(pieces).map(move |piece| {
            timer.record_progress();
            Ok::<_, std::io::Error>(piece)
        }))
    }

    pub(crate) async fn send_request_and_timeout_when_no_progress_for_idle_limit(
        &self,
        request: RequestBuilder,
    ) -> Result<Response, ConnectorError> {
        self.record_progress();
        let send = request.send();
        tokio::pin!(send);
        loop {
            let deadline = self.idle_deadline();
            tokio::select! {
                response = &mut send => return Ok(response?),
                () = tokio::time::sleep_until(deadline.into()) => {
                    if Instant::now() >= self.idle_deadline() {
                        return Err(self.stalled_error());
                    }
                }
            }
        }
    }

    pub(crate) fn response_body_byte_stream_failing_after_idle_timeout(
        self,
        response: Response,
    ) -> impl Stream<Item = Result<Bytes, ConnectorError>> {
        futures::stream::unfold(Some(Box::pin(response.bytes_stream())), move |body| {
            let timer = self.clone();
            async move {
                let mut body = body?;
                match tokio::time::timeout(timer.idle_limit, body.next()).await {
                    Ok(Some(piece)) => Some((piece.map_err(ConnectorError::from), Some(body))),
                    Ok(None) => None,
                    Err(_) => Some((Err(timer.stalled_error()), None)),
                }
            }
        })
    }
}

pub(crate) fn build_url(base: &str, path: &str) -> String {
    format!(
        "{}/api/v1/{}",
        base.trim_end_matches('/'),
        path.trim_start_matches('/')
    )
}

pub(crate) fn encode_url_component(value: impl std::fmt::Display) -> String {
    value
        .to_string()
        .bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

pub(crate) async fn decode_cli_poll(res: Response) -> Result<CliPollOutcome, ConnectorError> {
    let status = res.status();
    let bytes = res.bytes().await?;

    if let Ok(env) = serde_json::from_slice::<Envelope<String>>(&bytes)
        && !env.error
    {
        return Ok(CliPollOutcome::Token(env.message));
    }

    if let Ok(env) = serde_json::from_slice::<ErrorResponseBody>(&bytes) {
        return match env.code.as_str() {
            "cli_auth_pending" => Ok(CliPollOutcome::Pending),
            "cli_auth_expired" => Ok(CliPollOutcome::Expired),
            "cli_auth_denied" => Ok(CliPollOutcome::Denied),
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

pub(crate) async fn decode_downloaded_file(
    res: Response,
) -> Result<DownloadedBinaryFile, ConnectorError> {
    let status = res.status();
    if status == reqwest::StatusCode::UNAUTHORIZED {
        return Err(ConnectorError::Unauthorized);
    }
    if !status.is_success() {
        return Err(decode::<String>(res)
            .await
            .map_or_else(|e| e, |message| ConnectorError::Api { status, message }));
    }
    let suggested_filename = res
        .headers()
        .get(reqwest::header::CONTENT_DISPOSITION)
        .and_then(|value| value.to_str().ok())
        .and_then(content_disposition_filename);
    Ok(DownloadedBinaryFile {
        suggested_filename,
        bytes: res.bytes().await?,
    })
}

fn content_disposition_filename(header: &str) -> Option<String> {
    header
        .split(';')
        .find_map(|part| part.trim().strip_prefix("filename="))
        .map(|name| name.trim_matches('"').to_owned())
        .filter(|name| !name.is_empty())
}

pub(crate) async fn open_api_websocket(
    client: &Client,
    endpoint: &str,
) -> Result<WebSocketStream<MaybeTlsStream<TcpStream>>, ConnectorError> {
    let url = websocket_url(&build_url(client.base_url(), endpoint));
    let mut request = url.into_client_request().map_err(io_error)?;
    if let Some(token) = client.token() {
        let bearer = HeaderValue::from_str(&format!("Bearer {token}")).map_err(io_error)?;
        request.headers_mut().insert("Authorization", bearer);
    }
    let tls = Connector::Rustls(Arc::new(crate::rustls_config()));
    let (socket, _) =
        tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(tls))
            .await
            .map_err(io_error)?;
    Ok(socket)
}

pub(crate) async fn open_live_events_websocket_stream(
    client: &Client,
    endpoint: &str,
) -> Result<impl Stream<Item = Result<LiveUpdateEvent, ConnectorError>> + use<>, ConnectorError> {
    let socket = open_api_websocket(client, endpoint).await?;
    Ok(socket.filter_map(|message| {
        std::future::ready(match message {
            Ok(Message::Text(text)) => Some(serde_json::from_str(&text).map_err(Into::into)),
            Ok(_) => None,
            Err(e) => Some(Err(io_error(e))),
        })
    }))
}

fn websocket_url(http_url: &str) -> String {
    match http_url.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some(("http", rest)) => format!("ws://{rest}"),
        _ => http_url.to_owned(),
    }
}

pub(crate) fn io_error(error: impl std::error::Error + Send + Sync + 'static) -> ConnectorError {
    ConnectorError::Io(std::io::Error::other(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_supplied_name_stays_one_url_component() {
        assert_eq!(encode_url_component("web-app_1.x~"), "web-app_1.x~");
        assert_eq!(encode_url_component("a/b?c#d"), "a%2Fb%3Fc%23d");
        assert_eq!(encode_url_component("x&y=1 z"), "x%26y%3D1%20z");
        assert_eq!(encode_url_component("ü"), "%C3%BC");
        assert_eq!(encode_url_component(42), "42");
    }

    #[test]
    fn websockets_use_the_websocket_scheme_of_the_server() {
        assert_eq!(
            websocket_url("https://ci.example/api/v1/exec/j/shell?attach=true"),
            "wss://ci.example/api/v1/exec/j/shell?attach=true"
        );
        assert_eq!(
            websocket_url("http://localhost:3000/api/v1/x"),
            "ws://localhost:3000/api/v1/x"
        );
    }

    #[test]
    fn the_attachment_filename_comes_from_content_disposition() {
        assert_eq!(
            content_disposition_filename(r#"attachment; filename="report.db""#).as_deref(),
            Some("report.db")
        );
        assert_eq!(content_disposition_filename("attachment"), None);
    }
}
