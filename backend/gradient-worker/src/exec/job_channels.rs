/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::VecDeque;
use std::path::PathBuf;

use gradient_vm::ConsoleLink;
use tokio::sync::mpsc;
use tracing::debug;

use super::agent_client::AgentClient;
use super::channels::{GuestChannelCommand, GuestChannelMux, MuxAction};
use super::job::ExecReport;

pub type GuestChannelInbox = mpsc::Receiver<GuestChannelCommand>;

const NO_CONSOLE_SOCKET: &str = "this guest has no console socket";
const CONSOLE_CLOSED: &str = "the guest console closed";

pub enum ChannelEvent {
    Command(GuestChannelCommand),
    ConsoleOutput(Vec<u8>),
    ConsoleLost,
}

pub struct JobChannels {
    inbox: Option<GuestChannelInbox>,
    mux: GuestChannelMux,
    guest_dir: PathBuf,
    console: Option<ConsoleLink>,
}

impl JobChannels {
    pub fn new(inbox: GuestChannelInbox, mux: GuestChannelMux, guest_dir: PathBuf) -> Self {
        Self {
            inbox: Some(inbox),
            mux,
            guest_dir,
            console: None,
        }
    }

    pub async fn next_event(&mut self) -> ChannelEvent {
        loop {
            tokio::select! {
                command = next_command(self.inbox.as_mut()) => match command {
                    Some(command) => return ChannelEvent::Command(command),
                    None => self.inbox = None,
                },
                output = read_console(self.console.as_mut()) => {
                    return output.map_or(ChannelEvent::ConsoleLost, ChannelEvent::ConsoleOutput);
                }
            }
        }
    }

    pub async fn handle<R: ExecReport>(
        &mut self,
        event: ChannelEvent,
        agent: &mut AgentClient,
        reporter: &mut R,
    ) {
        let actions = match event {
            ChannelEvent::Command(command) => self.mux.on_command(command),
            ChannelEvent::ConsoleOutput(data) => self.mux.on_console_output(&data),
            ChannelEvent::ConsoleLost => self.lose_console(CONSOLE_CLOSED),
        };
        self.apply(actions, agent, reporter).await;
    }

    pub async fn agent_data<R: ExecReport>(
        &mut self,
        channel_id: u64,
        data: Vec<u8>,
        agent: &mut AgentClient,
        reporter: &mut R,
    ) {
        let actions = self.mux.on_agent_data(channel_id, data);
        self.apply(actions, agent, reporter).await;
    }

    pub async fn agent_closed<R: ExecReport>(
        &mut self,
        channel_id: u64,
        exit_code: Option<i32>,
        message: Option<String>,
        agent: &mut AgentClient,
        reporter: &mut R,
    ) {
        let actions = self.mux.on_agent_close(channel_id, exit_code, message);
        self.apply(actions, agent, reporter).await;
    }

    pub async fn end<R: ExecReport>(
        &mut self,
        exit_code: Option<i32>,
        agent: &mut AgentClient,
        reporter: &mut R,
    ) {
        let actions = self.mux.on_job_end(exit_code);
        self.apply(actions, agent, reporter).await;
        self.inbox = None;
    }

    fn lose_console(&mut self, message: &str) -> Vec<MuxAction> {
        self.console = None;
        self.mux.on_console_lost(message)
    }

    async fn apply<R: ExecReport>(
        &mut self,
        actions: Vec<MuxAction>,
        agent: &mut AgentClient,
        reporter: &mut R,
    ) {
        let mut pending = VecDeque::from(actions);
        while let Some(action) = pending.pop_front() {
            match action {
                MuxAction::ToAgent(message) => {
                    if let Err(e) = agent.send(&message).await {
                        debug!(error = %e, "a guest channel frame did not reach the agent");
                    }
                }
                MuxAction::ToServerData { channel_id, data } => {
                    reporter.channel_data(channel_id, data).await;
                }
                MuxAction::ToServerClose {
                    channel_id,
                    exit_code,
                    message,
                } => {
                    reporter
                        .channel_closed(channel_id, exit_code, message)
                        .await
                }
                MuxAction::ConsoleInput(data) => {
                    if !self.write_console(&data).await {
                        pending.extend(self.lose_console(CONSOLE_CLOSED));
                    }
                }
                MuxAction::ConnectConsole => match ConsoleLink::connect(&self.guest_dir).await {
                    Ok(link) => self.console = Some(link),
                    Err(e) => {
                        debug!(error = %format!("{e:#}"), "no console link to the guest");
                        pending.extend(self.lose_console(NO_CONSOLE_SOCKET));
                    }
                },
                MuxAction::DisconnectConsole => self.console = None,
            }
        }
    }

    async fn write_console(&mut self, data: &[u8]) -> bool {
        match self.console.as_mut() {
            Some(console) => console.write(data).await.is_ok(),
            None => true,
        }
    }
}

async fn next_command(inbox: Option<&mut GuestChannelInbox>) -> Option<GuestChannelCommand> {
    match inbox {
        Some(inbox) => inbox.recv().await,
        None => std::future::pending().await,
    }
}

async fn read_console(console: Option<&mut ConsoleLink>) -> Option<Vec<u8>> {
    match console {
        Some(console) => console.read().await,
        None => std::future::pending().await,
    }
}
