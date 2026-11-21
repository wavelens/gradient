/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::access::{Caller, ProjectItemAccessRequirement};
use crate::authorization::{
    MaybeApiKey, WebSocketAuthorizationRecheck, WebSocketUpgradeCredentialRecheckRequestExtractor,
};
use crate::endpoints::live::{
    STREAM_ACCESS_ENDED_CLOSE_REASON, send_websocket_message_to_browser_with_timeout,
    stream_access_lost_close_frame,
};
use crate::error::{WebError, WebResult};
use crate::extractors::{Path, Query};
use crate::permissions::Permission;
use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket};
use axum::http::Method;
use axum::response::Response;
use futures::{Sink, Stream, StreamExt};
use gradient_core::ServerState;
use gradient_db::WebDb;
use gradient_pool::guest_channels::{GuestChannelEvent, GuestChannelHandle};
use gradient_scheduler::{GuestChannelOpened, Scheduler};
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

#[derive(Debug, PartialEq, Eq, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ForwardQuery {
    pub node: Option<u32>,
    pub port: u16,
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
    path = "/exec/{job}/console",
    operation_id = "execJobConsole",
    tag = "exec-jobs",
    summary = "Exec job serial console (WebSocket)",
    description = "Upgrades to a WebSocket joined to the serial console of the job's guest. Opens while the job is `starting` or `running`, also with `ssh = false`.\n\n**Frames**\n- Binary frames carry raw terminal bytes both ways; every viewer first receives the last 64 KiB of console output.\n- The oldest open console holds the input; every further viewer is read-only.\n- Text frames ``{\"type\":\"resize\",\"cols\":120,\"rows\":40}`` from the viewer in control of input resize the console. Read-only viewers can't resize the console.\n- The last server frame is the text frame `{\"type\":\"closed\",\"exit_code\":null,\"message\":\"...\"}`, also when the channel is refused after the upgrade.\n\n`node` picks the node of a multi-node job by its index, node 0 by default; an unknown index closes with `no node <n> in this job`. Requires the `OpenJobTerminal` permission on the project.",
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
    credentials: WebSocketUpgradeCredentialRecheckRequestExtractor,
) -> WebResult<Response> {
    ExecJobAccessContext::load(
        &state.web_db,
        job,
        Caller::User(&user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::IncludesPermission(Permission::OpenJobTerminal),
    )
    .await?;
    let request = TerminalRequest {
        job,
        node: query.node.unwrap_or(0),
        kind: GuestChannelKind::Console,
    };
    Ok(upgrade(
        &state.shutdown,
        &state.web_db,
        scheduler,
        request,
        credentials,
    ))
}

#[utoipa::path(
    get,
    path = "/exec/{job}/shell",
    operation_id = "execJobShell",
    tag = "exec-jobs",
    summary = "Exec job shell (WebSocket)",
    description = "Upgrades to a WebSocket joined to a terminal in the job's guest. With `attach=true` it joins the program's terminal (the job needs `ssh = true`); without it a new shell starts as `gradient` in `/work` with the job's environment. Opens while the job is `running` and was not started with `ssh = false`.\n\n**Frames**\n- Binary frames carry raw terminal bytes both ways.\n- A text frame `{\"type\":\"resize\",\"cols\":120,\"rows\":40}` from the client resizes the terminal.\n- The last server frame is the text frame `{\"type\":\"closed\",\"exit_code\":0,\"message\":null}`, also when the channel is refused after the upgrade.\n\nClosing an attached terminal leaves the program running. `node` picks the node of a multi-node job by its index, node 0 by default; an unknown index closes with `no node <n> in this job`. Requires the `OpenJobTerminal` permission on the project.",
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
    credentials: WebSocketUpgradeCredentialRecheckRequestExtractor,
) -> WebResult<Response> {
    ExecJobAccessContext::load(
        &state.web_db,
        job,
        Caller::User(&user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::IncludesPermission(Permission::OpenJobTerminal),
    )
    .await?;
    let request = TerminalRequest {
        job,
        node: query.node.unwrap_or(0),
        kind: GuestChannelKind::Shell {
            pty: true,
            attach: query.attach,
        },
    };
    Ok(upgrade(
        &state.shutdown,
        &state.web_db,
        scheduler,
        request,
        credentials,
    ))
}

#[utoipa::path(
    get,
    path = "/exec/{job}/forward",
    operation_id = "execJobForward",
    tag = "exec-jobs",
    summary = "Exec job port forward (WebSocket)",
    description = "Upgrade to a WebSocket bound to one TCP connection to `port` inside the job guest. Sockets open while the job is `running`, was not started with `ssh = false` and lists `port` in `ports`.\n\n**Frames**\n- Binary frames carry the raw TCP bytes both ways.\n- The last server frame is the text frame `{\"type\":\"closed\",\"exit_code\":null,\"message\":\"...\"}`, also when the channel is refused after the upgrade.\n\nTCP connections each need their own WebSocket. Multi-node jobs use `node` to select a node by index (default node 0). Requests need the `OpenJobTerminal` permission on the project.",
    params(("job" = ExecJobId, Path), ForwardQuery),
    responses(
        (status = SWITCHING_PROTOCOLS, description = "Protocol switch - successful WebSocket upgrade"),
        WebError
    )
)]
pub async fn get_exec_job_forward(
    state: State<Arc<ServerState>>,
    Extension(user): Extension<MUser>,
    Extension(api_key): Extension<MaybeApiKey>,
    Extension(scheduler): Extension<Arc<Scheduler>>,
    Path(job): Path<ExecJobId>,
    Query(query): Query<ForwardQuery>,
    credentials: WebSocketUpgradeCredentialRecheckRequestExtractor,
) -> WebResult<Response> {
    ExecJobAccessContext::load(
        &state.web_db,
        job,
        Caller::User(&user),
        api_key.as_ref(),
        ProjectItemAccessRequirement::IncludesPermission(Permission::OpenJobTerminal),
    )
    .await?;
    let request = TerminalRequest {
        job,
        node: query.node.unwrap_or(0),
        kind: GuestChannelKind::Forward { port: query.port },
    };
    Ok(upgrade(
        &state.shutdown,
        &state.web_db,
        scheduler,
        request,
        credentials,
    ))
}

fn upgrade(
    shutdown: &Shutdown,
    web_db: &WebDb,
    scheduler: Arc<Scheduler>,
    request: TerminalRequest,
    credentials: WebSocketUpgradeCredentialRecheckRequestExtractor,
) -> Response {
    let (db, job) = (web_db.clone(), request.job);
    let guard = credentials.watch_caller_access_with_recheck(Method::POST, move |user, api_key| {
        let db = db.clone();
        async move {
            let user = user.ok_or_else(WebError::invalid_credentials)?;
            ExecJobAccessContext::load(
                &db,
                job,
                Caller::User(&user),
                api_key.as_ref(),
                ProjectItemAccessRequirement::IncludesPermission(Permission::OpenJobTerminal),
            )
            .await
            .map(drop)
        }
    });
    let cancel = shutdown.token();
    let shutdown = shutdown.clone();
    guard.on_upgrade(move |socket, guard| async move {
        let _ = shutdown
            .spawn(serve_terminal(socket, scheduler, request, guard, cancel))
            .await;
    })
}

async fn serve_terminal(
    socket: WebSocket,
    scheduler: Arc<Scheduler>,
    request: TerminalRequest,
    guard: WebSocketAuthorizationRecheck,
    cancel: CancellationToken,
) {
    let opened = scheduler
        .open_guest_channel(request.job, request.node, request.kind)
        .await;
    match opened {
        Ok(Ok(opened)) => {
            let (sink, stream) = socket.split();
            pipe_terminal(sink, stream, opened, guard.wait_until_access_lost(), cancel).await;
        }
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
    send_terminal_closed_message_then_websocket_close_frame(
        &mut sink,
        &closed,
        Message::Close(None),
    )
    .await;
}

async fn send_terminal_closed_message_then_websocket_close_frame<S>(
    sink: &mut S,
    closed: &TerminalEvent,
    close: Message,
) where
    S: Sink<Message> + Unpin,
{
    if send_websocket_message_to_browser_with_timeout(sink, closed_frame(closed)).await {
        send_websocket_message_to_browser_with_timeout(sink, close).await;
    }
}

async fn pipe_terminal<Si, St, E>(
    mut sink: Si,
    mut stream: St,
    opened: GuestChannelOpened,
    access_lost: impl Future<Output = ()>,
    cancel: CancellationToken,
) where
    Si: Sink<Message> + Unpin,
    St: Stream<Item = Result<Message, E>> + Unpin,
{
    let GuestChannelOpened { handle, mut events } = opened;
    tokio::pin!(access_lost);
    let closed = loop {
        tokio::select! {
            _ = cancel.cancelled() => return,
            _ = &mut access_lost => {
                let closed = TerminalEvent::Closed {
                    exit_code: None,
                    message: Some(STREAM_ACCESS_ENDED_CLOSE_REASON.into()),
                };
                send_terminal_closed_message_then_websocket_close_frame(
                    &mut sink,
                    &closed,
                    stream_access_lost_close_frame(),
                )
                .await;
                return;
            }
            incoming = stream.next() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(message)) => forward_client_message(&handle, message),
            },
            event = events.recv() => match event {
                Some(GuestChannelEvent::Data(data)) => {
                    if !send_websocket_message_to_browser_with_timeout(
                        &mut sink,
                        Message::Binary(data.into()),
                    )
                    .await
                    {
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
    send_terminal_closed_message_then_websocket_close_frame(
        &mut sink,
        &closed,
        Message::Close(None),
    )
    .await;
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
    use crate::endpoints::live::LIVE_STREAM_OR_JOB_TERMINAL_BROWSER_MESSAGE_SEND_TIMEOUT;
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
            axum::extract::Query::<TerminalQuery>::try_from_uri(&uri.parse::<Uri>().unwrap())
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

    #[tokio::test]
    async fn a_revoked_session_closes_an_open_terminal() {
        let (port, _signals) = tokio::sync::mpsc::unbounded_channel();
        let (handle, events) = gradient_pool::guest_channels::GuestChannels::new().open(
            "worker",
            Arc::new(port),
            "job".into(),
            GuestChannelKind::Console,
        );
        let (sink, mut sent) = futures::channel::mpsc::unbounded::<Message>();
        let client = futures::stream::pending::<Result<Message, axum::Error>>();

        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            pipe_terminal(
                sink,
                client,
                GuestChannelOpened { handle, events },
                std::future::ready(()),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("the terminal closes once the credentials are revoked");

        let closed = TerminalEvent::Closed {
            exit_code: None,
            message: Some(STREAM_ACCESS_ENDED_CLOSE_REASON.into()),
        };
        assert_eq!(sent.next().await, Some(closed_frame(&closed)));
        assert_eq!(sent.next().await, Some(stream_access_lost_close_frame()));
    }

    #[tokio::test(start_paused = true)]
    async fn job_terminal_closes_within_send_timeout_when_browser_stops_reading() {
        let (port, _signals) = tokio::sync::mpsc::unbounded_channel();
        let channels = gradient_pool::guest_channels::GuestChannels::new();
        let (handle, events) = channels.open(
            "worker",
            Arc::new(port),
            "job".into(),
            GuestChannelKind::Console,
        );
        let channel_id = handle.id();
        let (sink, _unread) = futures::channel::mpsc::channel::<Message>(0);
        let client = futures::stream::pending::<Result<Message, axum::Error>>();
        Shutdown::new().spawn(async move {
            for _ in 0..3 {
                channels.deliver("worker", channel_id, b"output".to_vec());
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }
            std::future::pending::<()>().await;
        });

        tokio::time::timeout(
            std::time::Duration::from_secs(3)
                + LIVE_STREAM_OR_JOB_TERMINAL_BROWSER_MESSAGE_SEND_TIMEOUT,
            pipe_terminal(
                sink,
                client,
                GuestChannelOpened { handle, events },
                std::future::pending::<()>(),
                CancellationToken::new(),
            ),
        )
        .await
        .expect("A terminal that the browser stopped reading closes at the send timeout.");
    }
}
