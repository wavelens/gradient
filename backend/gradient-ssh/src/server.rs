/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::auth::{authorize, fingerprint};
use crate::roots::Roots;
use crate::session::Session;
use gradient_core::ServerState;
use russh::keys::{PrivateKey, PublicKey};
use russh::server::{Auth, ChannelOpenHandle, Msg, Session as SshSession};
use russh::{Channel, ChannelId, MethodKind, MethodSet};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

pub fn config(host_key: PrivateKey) -> russh::server::Config {
    russh::server::Config {
        methods: MethodSet::from(&[MethodKind::PublicKey][..]),
        keys: vec![host_key],
        auth_rejection_time: Duration::from_secs(1),
        auth_rejection_time_initial: Some(Duration::ZERO),
        inactivity_timeout: Some(Duration::from_secs(3600)),
        ..Default::default()
    }
}

pub struct SshServer {
    state: Arc<ServerState>,
    roots: Arc<Roots>,
}

impl SshServer {
    pub fn new(state: Arc<ServerState>, roots: Arc<Roots>) -> Self {
        Self { state, roots }
    }
}

impl russh::server::Server for SshServer {
    type Handler = Connection;

    fn new_client(&mut self, _peer: Option<std::net::SocketAddr>) -> Connection {
        Connection {
            state: self.state.clone(),
            roots: self.roots.clone(),
            session: None,
            channels: HashMap::new(),
        }
    }
}

pub struct Connection {
    state: Arc<ServerState>,
    roots: Arc<Roots>,
    session: Option<Arc<Session>>,
    channels: HashMap<ChannelId, Channel<Msg>>,
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
        match authorize(&self.state, user, &fingerprint(key)).await {
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
                self.state.shutdown.spawn(crate::exec::run(
                    command,
                    channel,
                    handle,
                    user_session,
                    roots,
                ));
            }
            Err(error) => {
                self.state
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
        session.channel_failure(id)
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
