/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;

use anyhow::{Context, Result};
use axum::Router;
use axum::http::StatusCode;
use axum::routing::post;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};

const LOCAL_ADDRESS: &str = "127.0.0.1:7777";
const NOT_AVAILABLE: &str = "not available in this version";

pub async fn serve() -> Result<()> {
    let router = Router::new()
        .route("/phase", post(not_available))
        .route("/upload", post(not_available));
    let listener = TcpListener::bind(LOCAL_ADDRESS)
        .await
        .with_context(|| format!("listen on {LOCAL_ADDRESS}"))?;
    axum::serve(listener, router).await?;
    Ok(())
}

async fn not_available() -> (StatusCode, &'static str) {
    (StatusCode::NOT_IMPLEMENTED, NOT_AVAILABLE)
}

pub struct Answer {
    pub ok: bool,
    pub body: String,
}

pub async fn post_file(route: &str, query: &[(&str, &str)], file: Option<&Path>) -> Result<Answer> {
    let body = match file {
        Some(file) => tokio::fs::read(file)
            .await
            .with_context(|| format!("read {}", file.display()))?,
        None => Vec::new(),
    };
    let uri = format!("http://{LOCAL_ADDRESS}{route}?{}", query_string(query));

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

fn query_string(query: &[(&str, &str)]) -> String {
    query
        .iter()
        .map(|(key, value)| format!("{key}={}", percent_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn percent_encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
