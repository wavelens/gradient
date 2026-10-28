/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::require_project_permission;
use crate::authorization::MaybeApiKey;
use crate::error::{WebError, WebResult};
use crate::resource::Resource;
use axum::Extension;
use axum::extract::ws::rejection::WebSocketUpgradeRejection;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_pool::guest_channels::{GuestChannelEvent, GuestChannelHandle};
use gradient_scheduler::{GuestChannelOpened, Scheduler};
use gradient_types::permissions::Permission;
use gradient_types::*;
use gradient_util::shutdown::{CancellationToken, Shutdown};
use gradient_wire::types::GuestChannelKind;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

use super::ExecJobAccessContext;

const OPEN_FAILED: &str = "the terminal could not be opened";

#[derive(Debug, PartialEq, Eq, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ConsoleQuery {
    pub node: Option<u32>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TerminalQuery {
    pub node: Option<u32>,
    #[serde(default)]
    pub attach: bool,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TerminalControl {
    Resize { cols: u16, rows: u16 },
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum TerminalEvent {
    Closed {
        exit_code: Option<i32>,
        message: Option<String>,
    },
}

struct TerminalRequest {
    job: ExecJobId,
    node: u32,
    kind: GuestChannelKind,
}

#[utoipa::path(
    get,
    path = "/exec-jobs/{job}/console",
    operation_id = "execJobConsole",
    tag = "exec-jobs",
    summary = "Exec job serial console (WebSocket)",
    description = "Upgrades to a WebSocket joined to the serial console of the job's guest. Opens while the job is `starting` or `running`, also with `ssh = false`.\n\n**Frames**\n- Binary frames carry raw terminal bytes both ways; every viewer first receives the last 64 KiB of console output.\n- The oldest open console holds the input; every further viewer is read-only.\n- A text frame `{\"type\":\"resize\",\"cols\":120,\"rows\":40}` from the client is accepted and ignored by the console.\n- The last server frame is the text frame `{\"type\":\"closed\",\"exit_code\":null,\"message\":\"...\"}`, also when the channel is refused after the upgrade.\n\n`node` accepts `0` only. Requires the `TriggerEvaluation` permission on the project.",
    params(("job" = ExecJobId, Path), ConsoleQuery),
    responses(
        (status = SWITCHING_PROTOCOLS, description = "Switching Protocols - WebSocket upgrade successful"),
        WebError
    )
)]
pub async fn get_exec_job_console(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(job): Path<ExecJobId>,
    Query(query): Query<ConsoleQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> WebResult<Response> {
    authorize_terminal(&state.web_db, &user, &api_key, job).await?;
    let request = TerminalRequest {
        job,
        node: query.node.unwrap_or(0),
        kind: GuestChannelKind::Console,
    };
    Ok(upgrade(&state.shutdown, scheduler, ws, request))
}

#[utoipa::path(
    get,
    path = "/exec-jobs/{job}/shell",
    operation_id = "execJobShell",
    tag = "exec-jobs",
    summary = "Exec job shell (WebSocket)",
    description = "Upgrades to a WebSocket joined to a terminal in the job's guest. With `attach=true` it joins the program's terminal (the job needs `ssh = true`); without it a new shell starts as `gradient` in `/work` with the job's environment. Opens while the job is `running` and was not started with `ssh = false`.\n\n**Frames**\n- Binary frames carry raw terminal bytes both ways.\n- A text frame `{\"type\":\"resize\",\"cols\":120,\"rows\":40}` from the client resizes the terminal.\n- The last server frame is the text frame `{\"type\":\"closed\",\"exit_code\":0,\"message\":null}`, also when the channel is refused after the upgrade.\n\nClosing an attached terminal leaves the program running. `node` accepts `0` only. Requires the `TriggerEvaluation` permission on the project.",
    params(("job" = ExecJobId, Path), TerminalQuery),
    responses(
        (status = SWITCHING_PROTOCOLS, description = "Switching Protocols - WebSocket upgrade successful"),
        WebError
    )
)]
pub async fn get_exec_job_shell(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(job): Path<ExecJobId>,
    Query(query): Query<TerminalQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> WebResult<Response> {
    authorize_terminal(&state.web_db, &user, &api_key, job).await?;
    let request = TerminalRequest {
        job,
        node: query.node.unwrap_or(0),
        kind: GuestChannelKind::Shell {
            pty: true,
            attach: query.attach,
        },
    };
    Ok(upgrade(&state.shutdown, scheduler, ws, request))
}

async fn authorize_terminal(
    db: &WebDb,
    user: &MUser,
    api_key: &MaybeApiKey,
    job: ExecJobId,
) -> WebResult<()> {
    let ctx = ExecJobAccessContext::load_unguarded(db, job).await?;
    if user.superuser {
        return Ok(());
    }
    require_project_permission(
        db,
        user.id,
        ctx.project.id,
        Permission::TriggerEvaluation,
        Resource::ExecJob,
        api_key.as_ref(),
    )
    .await
}

fn upgrade(
    shutdown: &Shutdown,
    scheduler: Arc<Scheduler>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
    request: TerminalRequest,
) -> Response {
    let ws = match ws {
        Ok(ws) => ws,
        Err(rejection) => return rejection.into_response(),
    };
    let cancel = shutdown.token();
    let shutdown = shutdown.clone();
    ws.on_upgrade(move |socket| async move {
        let _ = shutdown
            .spawn(serve_terminal(socket, scheduler, request, cancel))
            .await;
    })
}

async fn serve_terminal(
    socket: WebSocket,
    scheduler: Arc<Scheduler>,
    request: TerminalRequest,
    cancel: CancellationToken,
) {
    let opened = scheduler
        .open_guest_channel(request.job, request.node, request.kind)
        .await;
    match opened {
        Ok(Ok(opened)) => pipe_terminal(socket, opened, cancel).await,
        Ok(Err(refusal)) => refuse(socket, refusal.to_string()).await,
        Err(e) => {
            tracing::warn!(exec_job = %request.job, error = %e, "opening a terminal failed");
            refuse(socket, OPEN_FAILED.into()).await;
        }
    }
}

async fn refuse(socket: WebSocket, message: String) {
    let (mut sink, _) = socket.split();
    let closed = TerminalEvent::Closed {
        exit_code: None,
        message: Some(message),
    };
    let _ = sink.send(closed_frame(&closed)).await;
    let _ = sink.send(Message::Close(None)).await;
}

pub async fn pipe_terminal(
    socket: WebSocket,
    opened: GuestChannelOpened,
    cancel: CancellationToken,
) {
    let GuestChannelOpened { handle, mut events } = opened;
    let (mut sink, mut stream) = socket.split();
    let closed = loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            incoming = stream.next() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(message)) => forward_client_message(&handle, message),
            },
            event = events.recv() => match event {
                Some(GuestChannelEvent::Data(data)) => {
                    if sink.send(Message::Binary(data.into())).await.is_err() {
                        return;
                    }
                }
                Some(GuestChannelEvent::Closed { exit_code, message }) => {
                    break TerminalEvent::Closed { exit_code, message };
                }
                None => break TerminalEvent::Closed { exit_code: None, message: None },
            },
        }
    };
    let _ = sink.send(closed_frame(&closed)).await;
    let _ = sink.send(Message::Close(None)).await;
}

fn forward_client_message(handle: &GuestChannelHandle, message: Message) {
    match message {
        Message::Binary(data) => handle.send(&data),
        Message::Text(text) => match serde_json::from_str::<TerminalControl>(&text) {
            Ok(TerminalControl::Resize { cols, rows }) => handle.resize(cols, rows),
            Err(e) => tracing::debug!(error = %e, "unknown terminal control frame"),
        },
        _ => {}
    }
}

fn closed_frame(event: &TerminalEvent) -> Message {
    let text = serde_json::to_string(event).unwrap_or_default();
    Message::Text(text.into())
}

pub fn ssh_commands(
    host: &str,
    port: u16,
    project: &str,
    job: ExecJobId,
    ports: &[u16],
) -> (String, Vec<String>) {
    let login = format!("{project}/{job}@{host}");
    let shell = format!("ssh -p {port} {login}");
    let forwards = ports
        .iter()
        .map(|p| format!("ssh -p {port} -N -L {p}:127.0.0.1:{p} {login}"))
        .collect();
    (shell, forwards)
}

pub(super) fn ssh_host(serve_url: &str) -> Option<String> {
    url::Url::parse(serve_url)
        .ok()?
        .host_str()
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Uri;
    use uuid::Uuid;

    #[test]
    fn a_resize_frame_parses_and_unknown_frames_are_refused() {
        assert_eq!(
            serde_json::from_str::<TerminalControl>(r#"{"type":"resize","cols":120,"rows":40}"#)
                .unwrap(),
            TerminalControl::Resize {
                cols: 120,
                rows: 40
            }
        );
        assert!(serde_json::from_str::<TerminalControl>(r#"{"type":"paste"}"#).is_err());
    }

    #[test]
    fn the_shell_query_picks_attach_or_a_new_shell() {
        let parse = |uri: &str| {
            Query::<TerminalQuery>::try_from_uri(&uri.parse::<Uri>().unwrap())
                .unwrap()
                .0
        };
        assert_eq!(
            parse("/shell?attach=true&node=0"),
            TerminalQuery {
                node: Some(0),
                attach: true
            }
        );
        assert_eq!(
            parse("/shell"),
            TerminalQuery {
                node: None,
                attach: false
            }
        );
    }

    #[test]
    fn ssh_commands_name_the_job_login_and_each_port() {
        let id = ExecJobId::new(Uuid::from_u128(0x7a));
        assert_eq!(
            ssh_commands("ci.example.com", 2222, "infra", id, &[6006]),
            (
                format!("ssh -p 2222 infra/{id}@ci.example.com"),
                vec![format!(
                    "ssh -p 2222 -N -L 6006:127.0.0.1:6006 infra/{id}@ci.example.com"
                )]
            )
        );
    }
}
