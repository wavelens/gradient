/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::auth::{authorize, fingerprint};
use crate::gateway::{TerminalSize, run_forward, run_shell};
use crate::gateway_user::{GatewayTarget, gateway_shell_kind};
use crate::roots::Roots;
use crate::session::{Session, SshHandles};
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session as SshSession};
use russh::{Channel, ChannelId, ChannelOpenFailure, MethodKind, MethodSet, Pty};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

pub fn config(host_key: PrivateKey) -> russh::server::Config {
    russh::server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        keys: vec![host_key],
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: Some(Duration::from_secs(3600)),
        preferred: gradient_util::ssh::ALGORITHMS,
        ..Default::default()
    }
}

pub struct SshServer {
    handles: SshHandles,
    roots: Arc<Roots>,
}

impl SshServer {
    pub fn new(handles: SshHandles, roots: Arc<Roots>) -> Self {
        Self { handles, roots }
    }
}

impl russh::server::Server for SshServer {
    type Handler = Connection;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Connection {
        Connection {
            handles: self.handles.clone(),
            roots: self.roots.clone(),
            session: None,
            channels: HashMap::new(),
            sizes: HashMap::new(),
        }
    }
}

pub struct Connection {
    handles: SshHandles,
    roots: Arc<Roots>,
    session: Option<Arc<Session>>,
    channels: HashMap<ChannelId, Channel<Msg>>,
    sizes: HashMap<ChannelId, watch::Sender<(u16, u16)>>,
}

fn terminal_size(cols: u32, rows: u32) -> (u16, u16) {
    let clamp = |value: u32| u16::try_from(value).unwrap_or(u16::MAX);
    (clamp(cols), clamp(rows))
}

impl Connection {
    fn gateway_target(&self) -> Option<(Arc<Session>, GatewayTarget)> {
        let session = self.session.clone()?;
        let target = session.target.clone()?;
        Some((session, target))
    }

    fn start_gateway_shell(
        &mut self,
        id: ChannelId,
        command: Option<&[u8]>,
        session: &mut SshSession,
    ) -> Result<(), russh::Error> {
        let Some((user_session, target)) = self.gateway_target() else {
            return session.channel_failure(id);
        };
        let Some(channel) = self.channels.remove(&id) else {
            return session.channel_failure(id);
        };
        session.channel_success(id)?;
        let sizes: Option<TerminalSize> = self.sizes.get(&id).map(watch::Sender::subscribe);
        match gateway_shell_kind(command, sizes.is_some()) {
            Ok(kind) => {
                self.handles.db.shutdown.spawn(run_shell(
                    channel,
                    user_session,
                    target,
                    kind,
                    sizes,
                ));
            }
            Err(message) => {
                self.handles.db.shutdown.spawn(crate::exec::refuse(
                    session.handle(),
                    id,
                    message.to_owned(),
                ));
            }
        }
        Ok(())
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        if let Some(session) = &self.session {
            session.closed.cancel();
        }
    }
}

impl russh::server::Handler for Connection {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, user: &str, key: &PublicKey) -> Result<Auth, Self::Error> {
        match authorize(&self.handles, user, &fingerprint(key)).await {
            Ok(session) => {
                self.session = Some(session);
                Ok(Auth::Accept)
            }
            Err(_) => Ok(Auth::reject()),
        }
    }

    async fn channel_open_session(
        &mut self,
        channel: Channel<Msg>,
        reply: ChannelOpenHandle,
        _session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(
        &mut self,
        id: ChannelId,
        data: &[u8],
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if self.gateway_target().is_some() {
            return self.start_gateway_shell(id, Some(data), session);
        }
        let (Some(channel), Some(user_session)) = (self.channels.remove(&id), self.session.clone())
        else {
            return session.channel_failure(id);
        };

        session.channel_success(id)?;
        let handle = session.handle();
        let line = String::from_utf8_lossy(data).into_owned();
        let roots = self.roots.clone();
        match crate::commands::parse(&line) {
            Ok(command) => {
                self.handles.db.shutdown.spawn(crate::exec::run(
                    command,
                    channel,
                    handle,
                    user_session,
                    roots,
                ));
            }
            Err(error) => {
                self.handles
                    .db
                    .shutdown
                    .spawn(crate::exec::refuse(handle, id, error.to_string()));
            }
        }

        Ok(())
    }

    async fn shell_request(
        &mut self,
        id: ChannelId,
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        self.start_gateway_shell(id, None, session)
    }

    async fn pty_request(
        &mut self,
        id: ChannelId,
        _term: &str,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _modes: &[(Pty, u32)],
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if self.gateway_target().is_none() {
            return Ok(());
        }
        let (sizes, _) = watch::channel(terminal_size(col_width, row_height));
        self.sizes.insert(id, sizes);
        session.channel_success(id)
    }

    async fn window_change_request(
        &mut self,
        id: ChannelId,
        col_width: u32,
        row_height: u32,
        _pix_width: u32,
        _pix_height: u32,
        _session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        if let Some(sizes) = self.sizes.get(&id) {
            sizes.send_replace(terminal_size(col_width, row_height));
        }
        Ok(())
    }

    async fn channel_open_direct_tcpip(
        &mut self,
        channel: Channel<Msg>,
        _host_to_connect: &str,
        port_to_connect: u32,
        _originator_address: &str,
        _originator_port: u32,
        reply: ChannelOpenHandle,
        _session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        let port = u16::try_from(port_to_connect).ok();
        match (self.gateway_target(), port) {
            (Some((user_session, target)), Some(port)) => {
                self.handles.db.shutdown.spawn(run_forward(
                    channel,
                    reply,
                    user_session,
                    target,
                    port,
                ));
            }
            _ => {
                reply
                    .reject(ChannelOpenFailure::AdministrativelyProhibited)
                    .await
            }
        }
        Ok(())
    }

    async fn channel_close(
        &mut self,
        id: ChannelId,
        _session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        self.channels.remove(&id);
        self.sizes.remove(&id);
        Ok(())
    }

    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        _name: &str,
        session: &mut SshSession,
    ) -> Result<(), Self::Error> {
        session.channel_failure(id)
    }
}
