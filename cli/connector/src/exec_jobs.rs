// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{ExecJobResponse, ExecJobSummary};
use crate::{Client, ConnectorError, http};
use futures::{SinkExt, StreamExt};
use reqwest::Method;
use serde::Deserialize;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Message, http::HeaderValue};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};

pub struct ExecJobsApi<'a>(pub(crate) &'a Client);

impl ExecJobsApi<'_> {
    pub async fn of_evaluation(
        &self,
        evaluation: &str,
    ) -> Result<Vec<ExecJobSummary>, ConnectorError> {
        self.get_json(&format!("evals/{evaluation}/exec")).await
    }

    pub async fn get(&self, job: &str) -> Result<ExecJobResponse, ConnectorError> {
        self.get_json(&format!("exec/{job}")).await
    }

    pub async fn log(&self, job: &str) -> Result<String, ConnectorError> {
        self.get_json(&format!("exec/{job}/log")).await
    }

    pub async fn abort(&self, job: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("exec/{job}/abort"),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn attach_program_terminal(
        &self,
        job: &str,
    ) -> Result<ProgramTerminal, ConnectorError> {
        let token = self.0.token().ok_or(ConnectorError::Unauthorized)?;
        let url = websocket_url(&http::build_url(
            self.0.base_url(),
            &format!("exec/{job}/shell?attach=true"),
        ));
        let mut request = url.into_client_request().map_err(io_error)?;
        let bearer = HeaderValue::from_str(&format!("Bearer {token}")).map_err(io_error)?;
        request.headers_mut().insert("Authorization", bearer);
        let tls = Connector::Rustls(Arc::new(crate::rustls_config()));
        let (socket, _) =
            tokio_tungstenite::connect_async_tls_with_config(request, None, false, Some(tls))
                .await
                .map_err(io_error)?;
        Ok(ProgramTerminal { socket })
    }

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        endpoint: &str,
    ) -> Result<T, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            endpoint,
            false,
        )?;
        http::decode(req.send().await?).await
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TerminalEvent {
    Output(Vec<u8>),
    Closed {
        exit_code: Option<i32>,
        message: Option<String>,
    },
}

pub struct ProgramTerminal {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl ProgramTerminal {
    pub async fn send_input(&mut self, bytes: Vec<u8>) -> Result<(), ConnectorError> {
        self.socket
            .send(Message::Binary(bytes.into()))
            .await
            .map_err(io_error)
    }

    pub async fn resize(&mut self, cols: u16, rows: u16) -> Result<(), ConnectorError> {
        let frame = serde_json::json!({"type": "resize", "cols": cols, "rows": rows});
        self.socket
            .send(Message::Text(frame.to_string().into()))
            .await
            .map_err(io_error)
    }

    pub async fn next_event(&mut self) -> Option<TerminalEvent> {
        while let Some(Ok(message)) = self.socket.next().await {
            match message {
                Message::Binary(bytes) => return Some(TerminalEvent::Output(bytes.to_vec())),
                Message::Text(text) => {
                    if let Some(closed) = closed_frame(&text) {
                        return Some(closed);
                    }
                }
                Message::Close(_) => return None,
                _ => {}
            }
        }
        None
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ServerFrame {
    Closed {
        exit_code: Option<i32>,
        message: Option<String>,
    },
}

fn closed_frame(text: &str) -> Option<TerminalEvent> {
    match serde_json::from_str(text).ok()? {
        ServerFrame::Closed { exit_code, message } => {
            Some(TerminalEvent::Closed { exit_code, message })
        }
    }
}

fn websocket_url(http_url: &str) -> String {
    match http_url.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some(("http", rest)) => format!("ws://{rest}"),
        _ => http_url.to_owned(),
    }
}

fn io_error(error: impl std::error::Error + Send + Sync + 'static) -> ConnectorError {
    ConnectorError::Io(std::io::Error::other(error))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_terminal_socket_uses_the_websocket_scheme_of_the_server() {
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
    fn only_the_closed_frame_ends_the_terminal() {
        assert_eq!(
            closed_frame(r#"{"type":"closed","exit_code":3,"message":null}"#),
            Some(TerminalEvent::Closed {
                exit_code: Some(3),
                message: None
            })
        );
        assert_eq!(closed_frame(r#"{"type":"resize","cols":1,"rows":1}"#), None);
    }
}
