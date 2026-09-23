/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::journal::Journal;
use harmonia_protocol::daemon::{
    DaemonResult, DaemonStore, FutureResultExt as _, HandshakeDaemonStore, ResultLog, TrustLevel,
};
use std::future::{Future, ready};
use std::sync::Arc;

#[derive(Debug, Clone, Copy)]
pub struct ConnInfo {
    pub id: u64,
    pub uid: Option<u32>,
}

pub trait Backend: Send + Sync + 'static {
    type Handler: HandshakeDaemonStore + Send + 'static;

    fn journal(&self) -> &Journal;
    fn handler(self: &Arc<Self>, conn: ConnInfo) -> Self::Handler;
    fn control(
        &self,
        cmd: &str,
        args: &serde_json::Value,
    ) -> Option<anyhow::Result<serde_json::Value>>;
}

#[derive(Debug, Clone, Copy)]
pub struct NullHandler;

impl HandshakeDaemonStore for NullHandler {
    type Store = Self;

    fn handshake(self) -> impl ResultLog<Output = DaemonResult<Self::Store>> + Send {
        ready(Ok(self)).empty_logs()
    }
}

impl DaemonStore for NullHandler {
    fn trust_level(&self) -> Option<TrustLevel> {
        None
    }

    fn shutdown(&mut self) -> impl Future<Output = DaemonResult<()>> + Send + '_ {
        ready(Ok(()))
    }
}
