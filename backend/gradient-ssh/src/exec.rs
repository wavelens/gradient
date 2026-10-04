/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::build_log::PlainLog;
use crate::commands::Command;
use crate::roots::Roots;
use crate::session::Session;
use crate::store::SshBackend;
use harmonia_protocol::log::LogMessage;
use russh::ChannelId;
use russh::server::{Handle, Msg};
use russh::{Channel, ChannelStream};
use std::fmt;
use std::path::Path;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::mpsc;

const STDERR: u32 = 1;

struct DaemonStream(ChannelStream<Msg>);

impl fmt::Debug for DaemonStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DaemonStream")
    }
}

impl AsyncRead for DaemonStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_read(cx, buf)
    }
}

impl AsyncWrite for DaemonStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0).poll_shutdown(cx)
    }
}

struct Output {
    handle: Handle,
    id: ChannelId,
}

impl Output {
    async fn stdout(&self, text: String) {
        let _ = self.handle.data(self.id, format!("{text}\n")).await;
    }

    async fn stderr(&self, text: String) {
        let _ = self
            .handle
            .extended_data(self.id, STDERR, format!("{text}\n"))
            .await;
    }

    async fn finish(&self, status: u32) {
        let _ = self.handle.exit_status_request(self.id, status).await;
        let _ = self.handle.eof(self.id).await;
        let _ = self.handle.close(self.id).await;
    }
}

pub async fn refuse(handle: Handle, id: ChannelId, message: String) {
    let output = Output { handle, id };
    output.stderr(message).await;
    output.finish(127).await;
}

pub async fn run(
    command: Command,
    channel: Channel<Msg>,
    handle: Handle,
    session: Arc<Session>,
    roots: Arc<Roots>,
) {
    let output = Output {
        handle,
        id: channel.id(),
    };
    let user = session.user.id;
    let status = match command {
        Command::Daemon => {
            let backend = Arc::new(SshBackend::new(session));
            let stream = DaemonStream(channel.into_stream());
            gradient_daemon::server::serve_stream(
                backend,
                gradient_daemon::server::next_conn(None),
                stream,
            )
            .await;
            0
        }
        Command::MakeTempDir { pattern } => {
            output
                .stdout(roots.make_dir(user, &pattern).display().to_string())
                .await;
            0
        }
        Command::ResolveLink { path } => match roots.resolve(user, Path::new(&path)) {
            Some(target) => {
                output.stdout(target).await;
                0
            }
            None => {
                output.stderr(format!("readlink: {path}: unknown")).await;
                1
            }
        },
        Command::RemoveDir { path } => {
            roots.remove_dir(user, Path::new(&path));
            0
        }
        Command::Realise {
            derivations,
            add_root,
        } => realise(&output, &session, &roots, &derivations, add_root).await,
        Command::Build { derivations } => {
            realise(&output, &session, &roots, &derivations, None).await
        }
    };

    output.finish(status).await;
}

async fn realise(
    output: &Output,
    session: &Session,
    roots: &Roots,
    derivations: &[String],
    add_root: Option<String>,
) -> u32 {
    let (tx, mut rx) = mpsc::unbounded_channel::<LogMessage>();
    let build = async move {
        crate::build_request::run(session, derivations, move |message| {
            let _ = tx.send(message);
        })
        .await
    };
    let forward = async {
        let mut plain = PlainLog::default();
        while let Some(message) = rx.recv().await {
            if let Some(line) = plain.line(message) {
                output.stderr(line).await;
            }
        }
    };
    let (outcome, ()) = tokio::join!(build, forward);

    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(error) => {
            output.stderr(format!("error: {error}")).await;
            return 1;
        }
    };

    let mut failed = false;
    let mut paths = Vec::new();
    for (_, result) in outcome.results {
        match result {
            Ok(outputs) => paths.extend(outputs.into_values()),
            Err(failure) => {
                failed = true;
                output.stderr(failure.message).await;
            }
        }
    }

    if failed {
        return 1;
    }

    match add_root {
        Some(root) => {
            let first = paths.first().cloned().unwrap_or_default();
            if !roots.add(session.user.id, Path::new(&root), first) {
                output
                    .stderr(format!("error: {root} is not inside a mktemp directory"))
                    .await;
                return 1;
            }

            output.stdout(root).await;
        }
        None => {
            for path in paths {
                output.stdout(path).await;
            }
        }
    }

    0
}
