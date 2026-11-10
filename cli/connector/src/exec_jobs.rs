// SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
// SPDX-License-Identifier: AGPL-3.0-only

use crate::api::{
    ExecJobLog, ExecJobOutputView, ExecJobPullRequestView, ExecJobResponse, ExecJobSummary,
};
use crate::http::encode_url_component;
use crate::http::io_error;
use crate::{Client, ConnectorError, http};
use futures::{SinkExt, Stream, StreamExt};
use reqwest::Method;
use serde::Deserialize;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

pub struct ExecJobsApi<'a>(pub(crate) &'a Client);

impl ExecJobsApi<'_> {
    pub async fn of_evaluation(
        &self,
        evaluation: &str,
    ) -> Result<Vec<ExecJobSummary>, ConnectorError> {
        self.get_json(&format!("evals/{}/exec", encode_url_component(evaluation)))
            .await
    }

    pub async fn get(&self, job: &str) -> Result<ExecJobResponse, ConnectorError> {
        self.get_json(&format!("exec/{}", encode_url_component(job)))
            .await
    }

    pub async fn log(&self, job: &str, offset: u64) -> Result<ExecJobLog, ConnectorError> {
        self.get_json(&format!(
            "exec/{}/log?offset={}",
            encode_url_component(job),
            encode_url_component(offset)
        ))
        .await
    }

    pub async fn abort(&self, job: &str) -> Result<String, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::POST,
            &format!("exec/{}/abort", encode_url_component(job)),
            true,
        )?;
        http::decode(req.send().await?).await
    }

    pub async fn attach_program_terminal(&self, job: &str) -> Result<GuestSocket, ConnectorError> {
        self.open_guest_socket(&format!(
            "exec/{}/shell?attach=true",
            encode_url_component(job)
        ))
        .await
    }

    pub async fn open_port_forward(
        &self,
        job: &str,
        port: u16,
    ) -> Result<GuestSocket, ConnectorError> {
        self.open_guest_socket(&format!(
            "exec/{}/forward?port={}",
            encode_url_component(job),
            encode_url_component(port)
        ))
        .await
    }

    pub async fn list_output_files(
        &self,
        job: &str,
        node: Option<u32>,
    ) -> Result<Vec<ExecJobOutputView>, ConnectorError> {
        let req = http::request(
            self.0.http(),
            self.0.base_url(),
            self.0.token(),
            Method::GET,
            &format!("exec/{}/outputs", encode_url_component(job)),
            true,
        )?
        .query(&node.map(|node| vec![("node", node)]).unwrap_or_default());
        http::decode(req.send().await?).await
    }

    pub async fn download_job_output_file_from_node(
        &self,
        job: &str,
        path: &str,
        node: Option<u32>,
    ) -> Result<impl Stream<Item = Result<bytes::Bytes, ConnectorError>> + use<>, ConnectorError>
    {
        let token = self.0.token().ok_or(ConnectorError::Unauthorized)?;
        let mut url = reqwest::Url::parse(&http::build_url(
            self.0.base_url(),
            &format!("exec/{}/outputs", encode_url_component(job)),
        ))
        .map_err(io_error)?;
        url.path_segments_mut()
            .map_err(|()| io_error(std::fmt::Error))?
            .push(path);
        if let Some(node) = node {
            url.query_pairs_mut().append_pair("node", &node.to_string());
        }
        let res = self
            .0
            .stream_http()
            .get(url)
            .bearer_auth(token)
            .send()
            .await?;
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
        Ok(res
            .bytes_stream()
            .map(|chunk| chunk.map_err(ConnectorError::from)))
    }

    pub async fn set_vm_keep_running(
        &self,
        job: &str,
        keep: bool,
    ) -> Result<String, ConnectorError> {
        let method = if keep { Method::POST } else { Method::DELETE };
        self.0
            .send_authenticated_request(
                method,
                &format!("exec/{}/keep-vm-running", encode_url_component(job)),
                None::<&()>,
            )
            .await
    }

    pub async fn get_exec_job_pull_requests(
        &self,
        job: &str,
    ) -> Result<Vec<ExecJobPullRequestView>, ConnectorError> {
        self.0
            .send_authenticated_request(
                Method::GET,
                &format!("exec/{}/pull-requests", encode_url_component(job)),
                None::<&()>,
            )
            .await
    }

    pub async fn open_job_serial_console_socket(
        &self,
        job: &str,
        node: Option<u32>,
    ) -> Result<GuestSocket, ConnectorError> {
        let query = node.map(|node| format!("?node={node}")).unwrap_or_default();
        self.open_guest_socket(&format!(
            "exec/{}/console{query}",
            encode_url_component(job)
        ))
        .await
    }

    async fn open_guest_socket(&self, endpoint: &str) -> Result<GuestSocket, ConnectorError> {
        self.0.token().ok_or(ConnectorError::Unauthorized)?;
        let socket = http::open_api_websocket(self.0, endpoint).await?;
        Ok(GuestSocket { socket })
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
pub enum GuestEvent {
    Output(Vec<u8>),
    Closed {
        exit_code: Option<i32>,
        message: Option<String>,
    },
}

pub struct GuestSocket {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
}

impl GuestSocket {
    pub async fn send_bytes(&mut self, bytes: Vec<u8>) -> Result<(), ConnectorError> {
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

    pub async fn next_event(&mut self) -> Option<GuestEvent> {
        while let Some(Ok(message)) = self.socket.next().await {
            match message {
                Message::Binary(bytes) => return Some(GuestEvent::Output(bytes.to_vec())),
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

fn closed_frame(text: &str) -> Option<GuestEvent> {
    match serde_json::from_str(text).ok()? {
        ServerFrame::Closed { exit_code, message } => {
            Some(GuestEvent::Closed { exit_code, message })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_closed_frame_ends_the_terminal() {
        assert_eq!(
            closed_frame(r#"{"type":"closed","exit_code":3,"message":null}"#),
            Some(GuestEvent::Closed {
                exit_code: Some(3),
                message: None
            })
        );
        assert_eq!(closed_frame(r#"{"type":"resize","cols":1,"rows":1}"#), None);
    }
}
