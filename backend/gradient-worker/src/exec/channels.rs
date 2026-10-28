/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{BTreeMap, VecDeque};

use gradient_redact::{SecretRedactor, StreamRedactor};
use gradient_wire::guest::{GUEST_CHANNELS_AGENT_SINCE, WorkerToAgent};
use gradient_wire::messages::{GUEST_CHANNEL_CHUNK, GuestChannelKind};

pub const CONSOLE_SCROLLBACK: usize = 64 * 1024;
const READ_ONLY_NOTICE: &[u8] =
    b"\r\n[gradient] another viewer holds the console input, this view is read-only\r\n";
const SSH_DISABLED: &str = "the exec job was started with ssh = false";
const NO_PROGRAM_TERMINAL: &str = "program has no terminal, set ssh = true";
const AGENT_TOO_OLD: &str =
    "the guest agent is too old for shells, the next attempt gets a new guest";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestChannelCommand {
    Open {
        channel_id: u64,
        kind: GuestChannelKind,
    },
    Data {
        channel_id: u64,
        data: Vec<u8>,
    },
    Resize {
        channel_id: u64,
        cols: u16,
        rows: u16,
    },
    Close {
        channel_id: u64,
    },
    CloseAll,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MuxAction {
    ToAgent(WorkerToAgent),
    ToServerData {
        channel_id: u64,
        data: Vec<u8>,
    },
    ToServerClose {
        channel_id: u64,
        exit_code: Option<i32>,
        message: Option<String>,
    },
    ConsoleInput(Vec<u8>),
    ConnectConsole,
    DisconnectConsole,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Route {
    Agent { attached: bool },
    Console,
}

pub struct GuestChannelMux {
    ssh: bool,
    ports: Vec<u16>,
    agent_version: u16,
    routes: BTreeMap<u64, Route>,
    console_viewers: Vec<u64>,
    scrollback: ConsoleScrollback,
    console_redactor: StreamRedactor,
}

impl GuestChannelMux {
    pub fn new(ssh: bool, ports: Vec<u16>, agent_version: u16) -> Self {
        Self {
            ssh,
            ports,
            agent_version,
            routes: BTreeMap::new(),
            console_viewers: Vec::new(),
            scrollback: ConsoleScrollback::default(),
            console_redactor: SecretRedactor::default().stream(),
        }
    }

    pub fn mask_console(&mut self, redactor: &SecretRedactor) {
        self.console_redactor = redactor.stream();
    }

    pub fn on_command(&mut self, command: GuestChannelCommand) -> Vec<MuxAction> {
        match command {
            GuestChannelCommand::Open { channel_id, kind } => self.open(channel_id, kind),
            GuestChannelCommand::Data { channel_id, data } => self.data(channel_id, data),
            GuestChannelCommand::Resize {
                channel_id,
                cols,
                rows,
            } => match self.routes.get(&channel_id) {
                Some(Route::Agent { .. }) => vec![MuxAction::ToAgent(WorkerToAgent::Resize {
                    channel_id,
                    cols,
                    rows,
                })],
                Some(Route::Console) | None => Vec::new(),
            },
            GuestChannelCommand::Close { channel_id } => self.close_from_server(channel_id),
            GuestChannelCommand::CloseAll => self.close_all(),
        }
    }

    pub fn on_agent_data(&mut self, channel_id: u64, data: Vec<u8>) -> Vec<MuxAction> {
        match self.routes.get(&channel_id) {
            Some(Route::Agent { .. }) => vec![MuxAction::ToServerData { channel_id, data }],
            Some(Route::Console) | None => Vec::new(),
        }
    }

    pub fn on_agent_close(
        &mut self,
        channel_id: u64,
        exit_code: Option<i32>,
        message: Option<String>,
    ) -> Vec<MuxAction> {
        if !matches!(self.routes.get(&channel_id), Some(Route::Agent { .. })) {
            return Vec::new();
        }
        self.routes.remove(&channel_id);
        vec![MuxAction::ToServerClose {
            channel_id,
            exit_code,
            message,
        }]
    }

    pub fn on_console_output(&mut self, data: &[u8]) -> Vec<MuxAction> {
        let masked = self.console_redactor.push(data);
        self.scrollback.push(&masked);
        self.to_console_viewers(&masked)
    }

    pub fn on_console_lost(&mut self, message: &str) -> Vec<MuxAction> {
        let rest = self.console_redactor.finish();
        let mut actions = self.to_console_viewers(&rest);
        self.scrollback.clear();
        let viewers = std::mem::take(&mut self.console_viewers);
        actions.extend(viewers.into_iter().map(|channel_id| {
            self.routes.remove(&channel_id);
            server_close(channel_id, Some(message.to_owned()))
        }));
        actions
    }

    fn to_console_viewers(&self, data: &[u8]) -> Vec<MuxAction> {
        self.console_viewers
            .iter()
            .flat_map(|&channel_id| data_actions(channel_id, data))
            .collect()
    }

    fn open(&mut self, channel_id: u64, kind: GuestChannelKind) -> Vec<MuxAction> {
        if kind == GuestChannelKind::Console {
            return self.open_console(channel_id);
        }
        if let Some(refusal) = self.refusal(&kind) {
            return vec![server_close(channel_id, Some(refusal))];
        }
        let attached = matches!(kind, GuestChannelKind::Shell { attach: true, .. });
        self.routes.insert(channel_id, Route::Agent { attached });
        vec![MuxAction::ToAgent(WorkerToAgent::OpenChannel {
            channel_id,
            kind,
        })]
    }

    fn refusal(&self, kind: &GuestChannelKind) -> Option<String> {
        match kind {
            GuestChannelKind::Shell { attach: true, .. } if !self.ssh => {
                Some(NO_PROGRAM_TERMINAL.to_owned())
            }
            _ if !self.ssh => Some(SSH_DISABLED.to_owned()),
            GuestChannelKind::Forward { port } if !self.ports.contains(port) => {
                Some(format!("port {port} is not in the job's ports"))
            }
            _ if self.agent_version < GUEST_CHANNELS_AGENT_SINCE => Some(AGENT_TOO_OLD.to_owned()),
            _ => None,
        }
    }

    fn open_console(&mut self, channel_id: u64) -> Vec<MuxAction> {
        let mut actions = Vec::new();
        if self.console_viewers.is_empty() {
            actions.push(MuxAction::ConnectConsole);
        }
        actions.extend(data_actions(channel_id, &self.scrollback.snapshot()));
        if !self.console_viewers.is_empty() {
            actions.extend(data_actions(channel_id, READ_ONLY_NOTICE));
        }
        self.console_viewers.push(channel_id);
        self.routes.insert(channel_id, Route::Console);
        actions
    }

    fn data(&mut self, channel_id: u64, data: Vec<u8>) -> Vec<MuxAction> {
        match self.routes.get(&channel_id) {
            Some(Route::Agent { .. }) => vec![MuxAction::ToAgent(WorkerToAgent::ChannelData {
                channel_id,
                data,
            })],
            Some(Route::Console) if self.console_viewers.first() == Some(&channel_id) => {
                vec![MuxAction::ConsoleInput(data)]
            }
            Some(Route::Console) | None => Vec::new(),
        }
    }

    fn close_from_server(&mut self, channel_id: u64) -> Vec<MuxAction> {
        match self.routes.remove(&channel_id) {
            Some(Route::Agent { .. }) => {
                vec![MuxAction::ToAgent(WorkerToAgent::CloseChannel {
                    channel_id,
                })]
            }
            Some(Route::Console) => self.leave_console(channel_id),
            None => Vec::new(),
        }
    }

    fn leave_console(&mut self, channel_id: u64) -> Vec<MuxAction> {
        self.console_viewers.retain(|&viewer| viewer != channel_id);
        if !self.console_viewers.is_empty() {
            return Vec::new();
        }
        self.end_console();
        vec![MuxAction::DisconnectConsole]
    }

    fn close_all(&mut self) -> Vec<MuxAction> {
        let mut actions = Vec::new();
        for (channel_id, route) in std::mem::take(&mut self.routes) {
            if matches!(route, Route::Agent { .. }) {
                actions.push(MuxAction::ToAgent(WorkerToAgent::CloseChannel {
                    channel_id,
                }));
            }
            actions.push(server_close(channel_id, None));
        }
        actions.extend(self.forget_console());
        actions
    }

    pub fn on_job_end(&mut self, exit_code: Option<i32>) -> Vec<MuxAction> {
        let mut actions: Vec<MuxAction> = std::mem::take(&mut self.routes)
            .into_iter()
            .map(|(channel_id, route)| MuxAction::ToServerClose {
                channel_id,
                exit_code: exit_code.filter(|_| route == Route::Agent { attached: true }),
                message: None,
            })
            .collect();
        actions.extend(self.forget_console());
        actions
    }

    fn forget_console(&mut self) -> Option<MuxAction> {
        if std::mem::take(&mut self.console_viewers).is_empty() {
            return None;
        }
        self.end_console();
        Some(MuxAction::DisconnectConsole)
    }

    fn end_console(&mut self) {
        self.scrollback.clear();
        self.console_redactor.finish();
    }
}

fn server_close(channel_id: u64, message: Option<String>) -> MuxAction {
    MuxAction::ToServerClose {
        channel_id,
        exit_code: None,
        message,
    }
}

fn data_actions(channel_id: u64, data: &[u8]) -> Vec<MuxAction> {
    data.chunks(GUEST_CHANNEL_CHUNK)
        .map(|chunk| MuxAction::ToServerData {
            channel_id,
            data: chunk.to_vec(),
        })
        .collect()
}

#[derive(Default)]
pub struct ConsoleScrollback {
    bytes: VecDeque<u8>,
}

impl ConsoleScrollback {
    pub fn push(&mut self, data: &[u8]) {
        self.bytes.extend(data);
        let excess = self.bytes.len().saturating_sub(CONSOLE_SCROLLBACK);
        self.bytes.drain(..excess);
    }

    pub fn snapshot(&self) -> Vec<u8> {
        self.bytes.iter().copied().collect()
    }

    fn clear(&mut self) {
        self.bytes.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mux() -> GuestChannelMux {
        GuestChannelMux::new(true, vec![6006], GUEST_CHANNELS_AGENT_SINCE)
    }

    fn open(channel_id: u64, kind: GuestChannelKind) -> GuestChannelCommand {
        GuestChannelCommand::Open { channel_id, kind }
    }

    fn data(channel_id: u64, data: &[u8]) -> GuestChannelCommand {
        GuestChannelCommand::Data {
            channel_id,
            data: data.to_vec(),
        }
    }

    fn to_server(channel_id: u64, data: &[u8]) -> MuxAction {
        MuxAction::ToServerData {
            channel_id,
            data: data.to_vec(),
        }
    }

    fn new_shell() -> GuestChannelKind {
        GuestChannelKind::Shell {
            pty: true,
            attach: false,
        }
    }

    fn refused(channel_id: u64, message: &str) -> Vec<MuxAction> {
        vec![server_close(channel_id, Some(message.to_owned()))]
    }

    #[test]
    fn a_shell_is_routed_to_the_agent_by_its_id() {
        let mut mux = mux();

        assert_eq!(
            mux.on_command(open(4, new_shell())),
            [MuxAction::ToAgent(WorkerToAgent::OpenChannel {
                channel_id: 4,
                kind: new_shell()
            })]
        );
        assert_eq!(mux.on_agent_data(4, b"$ ".to_vec()), [to_server(4, b"$ ")]);
        assert!(mux.on_command(data(9, b"x")).is_empty());
    }

    #[test]
    fn a_resize_reaches_shells_but_not_the_console() {
        let mut mux = mux();
        mux.on_command(open(4, new_shell()));
        mux.on_command(open(5, GuestChannelKind::Console));
        let resize = |channel_id| GuestChannelCommand::Resize {
            channel_id,
            cols: 120,
            rows: 40,
        };

        assert_eq!(
            mux.on_command(resize(4)),
            [MuxAction::ToAgent(WorkerToAgent::Resize {
                channel_id: 4,
                cols: 120,
                rows: 40
            })]
        );
        assert!(mux.on_command(resize(5)).is_empty());
    }

    #[test]
    fn a_close_from_the_server_reaches_the_agent_and_ends_the_route() {
        let mut mux = mux();
        mux.on_command(open(4, new_shell()));

        assert_eq!(
            mux.on_command(GuestChannelCommand::Close { channel_id: 4 }),
            [MuxAction::ToAgent(WorkerToAgent::CloseChannel {
                channel_id: 4
            })]
        );
        assert!(mux.on_agent_data(4, b"late".to_vec()).is_empty());
    }

    #[test]
    fn a_close_from_the_agent_reaches_the_server_with_its_exit_code() {
        let mut mux = mux();
        mux.on_command(open(4, new_shell()));

        assert_eq!(
            mux.on_agent_close(4, Some(0), None),
            [MuxAction::ToServerClose {
                channel_id: 4,
                exit_code: Some(0),
                message: None
            }]
        );
        assert!(mux.on_command(data(4, b"x")).is_empty());
    }

    #[test]
    fn only_declared_ports_are_forwarded() {
        let mut mux = mux();

        assert!(matches!(
            mux.on_command(open(5, GuestChannelKind::Forward { port: 6006 }))[..],
            [MuxAction::ToAgent(WorkerToAgent::OpenChannel {
                channel_id: 5,
                ..
            })]
        ));
        assert_eq!(
            mux.on_command(open(6, GuestChannelKind::Forward { port: 22 })),
            refused(6, "port 22 is not in the job's ports")
        );
    }

    #[test]
    fn a_job_without_ssh_gets_no_shell_but_a_console() {
        let mut mux = GuestChannelMux::new(false, Vec::new(), GUEST_CHANNELS_AGENT_SINCE);
        let attach = GuestChannelKind::Shell {
            pty: true,
            attach: true,
        };

        assert_eq!(
            mux.on_command(open(1, new_shell())),
            refused(1, "the exec job was started with ssh = false")
        );
        assert_eq!(
            mux.on_command(open(2, attach)),
            refused(2, "program has no terminal, set ssh = true")
        );
        assert_eq!(
            mux.on_command(open(3, GuestChannelKind::Console)),
            [MuxAction::ConnectConsole]
        );
    }

    #[test]
    fn an_old_agent_gets_no_shell() {
        let mut mux = GuestChannelMux::new(true, Vec::new(), GUEST_CHANNELS_AGENT_SINCE - 1);

        assert_eq!(
            mux.on_command(open(1, new_shell())),
            refused(
                1,
                "the guest agent is too old for shells, the next attempt gets a new guest"
            )
        );
    }

    #[test]
    fn a_second_console_viewer_is_read_only() {
        let mut mux = mux();
        mux.on_console_output(b"login: ");

        assert_eq!(
            mux.on_command(open(1, GuestChannelKind::Console)),
            [MuxAction::ConnectConsole, to_server(1, b"login: ")]
        );
        assert_eq!(
            mux.on_command(open(2, GuestChannelKind::Console)),
            [to_server(2, b"login: "), to_server(2, READ_ONLY_NOTICE)]
        );
        assert!(mux.on_command(data(2, b"root\n")).is_empty());
        assert_eq!(
            mux.on_command(data(1, b"root\n")),
            [MuxAction::ConsoleInput(b"root\n".to_vec())]
        );
    }

    #[test]
    fn the_next_console_viewer_takes_the_input_when_the_holder_leaves() {
        let mut mux = mux();
        mux.on_command(open(1, GuestChannelKind::Console));
        mux.on_command(open(2, GuestChannelKind::Console));

        assert!(
            mux.on_command(GuestChannelCommand::Close { channel_id: 1 })
                .is_empty()
        );
        assert_eq!(
            mux.on_command(data(2, b"x")),
            [MuxAction::ConsoleInput(b"x".to_vec())]
        );
        assert_eq!(
            mux.on_command(GuestChannelCommand::Close { channel_id: 2 }),
            [MuxAction::DisconnectConsole]
        );
    }

    #[test]
    fn a_lost_console_closes_its_viewers_and_the_next_one_connects_again() {
        let mut mux = mux();
        mux.on_command(open(1, GuestChannelKind::Console));
        mux.on_console_output(b"boot");

        assert_eq!(
            mux.on_console_lost("this guest has no console socket"),
            refused(1, "this guest has no console socket")
        );
        assert!(mux.on_command(data(1, b"x")).is_empty());
        assert_eq!(
            mux.on_command(open(2, GuestChannelKind::Console)),
            [MuxAction::ConnectConsole]
        );
    }

    #[test]
    fn close_all_ends_every_channel_towards_both_sides() {
        let mut mux = mux();
        mux.on_command(open(1, new_shell()));
        mux.on_command(open(2, GuestChannelKind::Console));

        assert_eq!(
            mux.on_command(GuestChannelCommand::CloseAll),
            [
                MuxAction::ToAgent(WorkerToAgent::CloseChannel { channel_id: 1 }),
                server_close(1, None),
                server_close(2, None),
                MuxAction::DisconnectConsole,
            ]
        );
    }

    #[test]
    fn the_job_end_gives_attached_channels_the_program_exit_code() {
        let mut mux = mux();
        let attach = GuestChannelKind::Shell {
            pty: true,
            attach: true,
        };
        mux.on_command(open(1, attach));
        mux.on_command(open(2, new_shell()));

        assert_eq!(
            mux.on_job_end(Some(0)),
            [
                MuxAction::ToServerClose {
                    channel_id: 1,
                    exit_code: Some(0),
                    message: None
                },
                server_close(2, None),
            ]
        );
        assert!(mux.on_agent_data(1, b"late".to_vec()).is_empty());
    }

    #[test]
    fn the_console_is_masked_across_chunks_for_every_viewer() {
        let mut mux = mux();
        mux.mask_console(&SecretRedactor::new([(
            "wandb-token",
            b"s3cr3t-value".as_slice(),
        )]));
        mux.on_command(open(1, GuestChannelKind::Console));

        let mut actions = mux.on_console_output(b"tok s3cr3t");
        actions.extend(mux.on_console_output(b"-value\n"));
        let seen: Vec<u8> = actions
            .iter()
            .flat_map(|action| match action {
                MuxAction::ToServerData {
                    channel_id: 1,
                    data,
                } => data.clone(),
                _ => Vec::new(),
            })
            .collect();
        let second_viewer = mux.on_command(open(2, GuestChannelKind::Console));

        assert_eq!(seen, b"tok [secret:wandb-token]\n");
        assert_eq!(
            second_viewer[0],
            to_server(2, b"tok [secret:wandb-token]\n")
        );
    }

    #[test]
    fn scrollback_keeps_the_newest_bytes() {
        let mut scrollback = ConsoleScrollback::default();
        scrollback.push(&vec![b'a'; 100 * 1024]);
        scrollback.push(b"tail");

        let snapshot = scrollback.snapshot();
        assert_eq!(snapshot.len(), CONSOLE_SCROLLBACK);
        assert!(snapshot.ends_with(b"tail"));
    }
}
