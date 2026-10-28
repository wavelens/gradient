/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use axum::Router;
use axum::body::Body;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use bytes::Bytes;
use gradient_redact::SecretRedactor;
use gradient_wire::guest::AgentToWorker;
use gradient_wire::types::{CONTINUE_OUTPUT, MAX_CONTINUE_FILE_BYTES};
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use tokio::net::{TcpListener, TcpStream};

use crate::output::OutputLog;
use crate::phase::{PhaseRefusal, check_phase, phase_marker};
use crate::upload::{UploadFailure, UploadSender, WorkerLink, output_refusal};

const LOCAL_ADDRESS: &str = "127.0.0.1:7777";
const NO_JOB: &str = "no job runs in this guest yet";

pub struct HttpState {
    pub link: WorkerLink,
    pub uploads: UploadSender,
    pub output: tokio::sync::Mutex<OutputLog>,
    redactor: Mutex<Option<SecretRedactor>>,
}

impl HttpState {
    pub fn new(output: OutputLog) -> Self {
        Self {
            link: WorkerLink::default(),
            uploads: UploadSender::default(),
            output: tokio::sync::Mutex::new(output),
            redactor: Mutex::new(None),
        }
    }

    pub fn hold_redactor(&self, redactor: SecretRedactor) {
        *self
            .redactor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(redactor);
    }

    fn redactor(&self) -> Option<SecretRedactor> {
        self.redactor
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}

pub async fn serve(state: Arc<HttpState>) -> Result<()> {
    let listener = TcpListener::bind(LOCAL_ADDRESS)
        .await
        .with_context(|| format!("listen on {LOCAL_ADDRESS}"))?;
    axum::serve(listener, router(state)).await?;
    Ok(())
}

fn router(state: Arc<HttpState>) -> Router {
    Router::new()
        .route("/phases", post(post_phase))
        .route("/outputs/{*path}", post(post_output))
        .route("/redact", post(post_redact))
        .with_state(state)
}

#[derive(Deserialize)]
struct PhaseQuery {
    name: String,
}

async fn post_phase(
    State(state): State<Arc<HttpState>>,
    Query(PhaseQuery { name }): Query<PhaseQuery>,
    payload: Bytes,
) -> Response {
    match check_phase(&name, &payload) {
        Err(refused @ PhaseRefusal::Name) => return refusal(StatusCode::BAD_REQUEST, refused),
        Err(refused @ PhaseRefusal::TooLarge(_)) => {
            return refusal(StatusCode::PAYLOAD_TOO_LARGE, refused);
        }
        Ok(()) => {}
    }
    let Some(redactor) = state.redactor() else {
        return refusal(StatusCode::SERVICE_UNAVAILABLE, NO_JOB);
    };
    let Some(worker) = state.link.current() else {
        return refusal(StatusCode::SERVICE_UNAVAILABLE, UploadFailure::Lost);
    };
    let payload = redactor.redact(&payload);
    if let Err(e) = state
        .output
        .lock()
        .await
        .append(phase_marker(&name).as_bytes())
        .await
    {
        tracing::warn!(error = %e, "cannot keep the phase marker");
    }
    match worker
        .outgoing
        .send(AgentToWorker::Phase { name, payload })
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(_) => refusal(StatusCode::SERVICE_UNAVAILABLE, UploadFailure::Lost),
    }
}

async fn post_output(
    State(state): State<Arc<HttpState>>,
    UrlPath(path): UrlPath<String>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Some(reason) = output_refusal(&path) {
        return refusal(StatusCode::BAD_REQUEST, reason);
    }
    let Some(size) = content_length(&headers) else {
        return refusal(
            StatusCode::LENGTH_REQUIRED,
            "an output upload needs a Content-Length",
        );
    };
    let Some(redactor) = state.redactor() else {
        return refusal(StatusCode::SERVICE_UNAVAILABLE, NO_JOB);
    };
    let uploaded = match path.as_str() {
        CONTINUE_OUTPUT => upload_continue_file(&state, &redactor, size, body).await,
        _ => {
            let stream = body.into_data_stream();
            state.uploads.upload(&state.link, &path, size, stream).await
        }
    };
    match uploaded {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(failure @ UploadFailure::Lost) => refusal(StatusCode::SERVICE_UNAVAILABLE, failure),
        Err(failure @ UploadFailure::Refused(_)) => refusal(StatusCode::BAD_GATEWAY, failure),
        Err(failure @ UploadFailure::Body(_)) => refusal(StatusCode::BAD_REQUEST, failure),
    }
}

async fn upload_continue_file(
    state: &HttpState,
    redactor: &SecretRedactor,
    size: u64,
    body: Body,
) -> Result<(), UploadFailure> {
    if size > MAX_CONTINUE_FILE_BYTES as u64 {
        return Err(UploadFailure::Refused(format!(
            "{CONTINUE_OUTPUT} is {size} bytes, the limit is {MAX_CONTINUE_FILE_BYTES}"
        )));
    }
    let read = axum::body::to_bytes(body, MAX_CONTINUE_FILE_BYTES)
        .await
        .map_err(|e| UploadFailure::Body(e.to_string()))?;
    let masked = Bytes::from(redactor.redact(&read));
    state
        .uploads
        .upload_bytes(&state.link, CONTINUE_OUTPUT, masked)
        .await
}

async fn post_redact(State(state): State<Arc<HttpState>>, body: Bytes) -> Response {
    match state.redactor() {
        Some(redactor) => (StatusCode::OK, redactor.redact(&body)).into_response(),
        None => refusal(StatusCode::SERVICE_UNAVAILABLE, NO_JOB),
    }
}

fn content_length(headers: &HeaderMap) -> Option<u64> {
    headers
        .get(header::CONTENT_LENGTH)?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

fn refusal(status: StatusCode, reason: impl ToString) -> Response {
    (status, reason.to_string()).into_response()
}

pub struct Answer {
    pub ok: bool,
    pub body: String,
}

pub async fn post_file(route: &str, file: Option<&Path>) -> Result<Answer> {
    let body = match file {
        Some(file) => tokio::fs::read(file)
            .await
            .with_context(|| format!("read {}", file.display()))?,
        None => Vec::new(),
    };
    let uri = format!("http://{LOCAL_ADDRESS}{route}");

    let stream = TcpStream::connect(LOCAL_ADDRESS)
        .await
        .context("the guest agent does not answer")?;
    let (mut sender, connection) =
        hyper::client::conn::http1::handshake(TokioIo::new(stream)).await?;
    let request = Request::post(uri)
        .header("host", LOCAL_ADDRESS)
        .body(Full::new(Bytes::from(body)))?;
    let exchange = async move {
        let response = sender.send_request(request).await?;
        let ok = response.status().is_success();
        let body = response.into_body().collect().await?.to_bytes();
        anyhow::Ok(Answer {
            ok,
            body: String::from_utf8_lossy(&body).into_owned(),
        })
    };
    let (answer, _) = tokio::join!(exchange, connection);
    answer
}

pub fn phase_route(name: &str) -> String {
    format!("/phases?name={}", percent_encode(name, b""))
}

pub fn output_route(path: &str) -> String {
    format!("/outputs/{}", percent_encode(path, b"/"))
}

fn percent_encode(value: &str, kept: &[u8]) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b if kept.contains(&b) => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use axum::http::Request;
    use gradient_wire::guest::GUEST_PROTO_VERSIONS;
    use tokio::sync::mpsc;
    use tower::ServiceExt;

    use super::*;

    async fn job_state(dir: &tempfile::TempDir) -> Arc<HttpState> {
        let output = OutputLog::create(&dir.path().join("output.log"), 1024 * 1024)
            .await
            .unwrap();
        let state = Arc::new(HttpState::new(output));
        state.hold_redactor(SecretRedactor::new([(
            "wandb-token",
            b"s3cr3t-value".as_slice(),
        )]));
        state
    }

    async fn answer(state: Arc<HttpState>, uri: &str, body: &'static [u8]) -> (StatusCode, Bytes) {
        let request = Request::post(uri).body(Body::from(body)).unwrap();
        let response = router(state).oneshot(request).await.unwrap();
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, body)
    }

    #[tokio::test]
    async fn a_phase_post_reaches_the_worker_and_the_log() {
        let dir = tempfile::tempdir().unwrap();
        let state = job_state(&dir).await;
        let (outgoing, mut sent) = mpsc::channel(4);
        state.link.connect(outgoing, *GUEST_PROTO_VERSIONS.end());

        let (status, _) = answer(
            Arc::clone(&state),
            "/phases?name=epoch-3",
            b"ckpt s3cr3t-value",
        )
        .await;

        assert_eq!(status, StatusCode::NO_CONTENT);
        assert_eq!(
            sent.recv().await,
            Some(AgentToWorker::Phase {
                name: "epoch-3".into(),
                payload: b"ckpt [secret:wandb-token]".to_vec(),
            })
        );
        let log = tokio::fs::read(dir.path().join("output.log"))
            .await
            .unwrap();
        assert!(log.ends_with(b"::gradient-phase:: epoch-3\n"));
    }

    #[tokio::test]
    async fn the_redact_endpoint_masks_a_whole_body() {
        let dir = tempfile::tempdir().unwrap();
        let state = job_state(&dir).await;

        let (status, body) = answer(state, "/redact", b"+token = \"s3cr3t-value\"\n").await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(&body[..], b"+token = \"[secret:wandb-token]\"\n");
    }
}
