/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use gradient_redact::StreamRedactor;
use gradient_util::shutdown::Shutdown;
use gradient_wire::guest::AgentToWorker;
use gradient_wire::messages::{GUEST_CHANNEL_CHUNK, GuestChannelKind};
use nix::sys::signal::Signal;
use nix::unistd::Pid;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::output::{OutputChunk, OutputReader};
use crate::run::{exit_code, signal_group};
use crate::shell::{Shell, ShellContext, spawn_shell};
use crate::terminal::{ProgramTerminal, PtyMaster};

pub const NO_PROGRAM_TERMINAL: &str = "program has no terminal, set ssh = true";
const INPUT_QUEUE: usize = 256;
const HANG_UP_GRACE: Duration = Duration::from_secs(2);
const OUTPUT_DRAIN: Duration = Duration::from_secs(2);

type Input = Box<dyn AsyncWrite + Send + Unpin>;

pub struct AgentChannels {
    open: HashMap<u64, OpenChannel>,
    outbound: mpsc::Sender<AgentToWorker>,
    tasks: Shutdown,
}

struct OpenChannel {
    input: mpsc::Sender<Vec<u8>>,
    terminal: Option<Arc<PtyMaster>>,
    ended: CancellationToken,
}

impl AgentChannels {
    pub fn new(outbound: mpsc::Sender<AgentToWorker>, tasks: Shutdown) -> Self {
        Self {
            open: HashMap::new(),
            outbound,
            tasks,
        }
    }

    pub async fn open(
        &mut self,
        channel_id: u64,
        kind: GuestChannelKind,
        context: Option<&ShellContext>,
    ) {
        self.open.retain(|_, channel| !channel.ended.is_cancelled());
        match self.start(channel_id, kind, context).await {
            Ok(channel) => {
                self.open.insert(channel_id, channel);
            }
            Err(message) => self.refuse(channel_id, message).await,
        }
    }

    pub async fn data(&mut self, channel_id: u64, data: Vec<u8>) {
        let Some(channel) = self.open.get(&channel_id) else {
            return;
        };
        if channel.input.try_send(data).is_ok() {
            return;
        }
        let ended = channel.ended.is_cancelled();
        self.close(channel_id);
        if !ended {
            self.refuse(
                channel_id,
                "the guest stopped reading the channel input".to_owned(),
            )
            .await;
        }
    }

    pub fn resize(&mut self, channel_id: u64, cols: u16, rows: u16) {
        let terminal = self
            .open
            .get(&channel_id)
            .and_then(|channel| channel.terminal.as_ref());
        if let Some(terminal) = terminal
            && let Err(e) = terminal.resize(cols, rows)
        {
            tracing::debug!(channel_id, error = %e, "cannot resize a channel terminal");
        }
    }

    pub fn close(&mut self, channel_id: u64) {
        if let Some(channel) = self.open.remove(&channel_id) {
            channel.ended.cancel();
        }
    }

    pub fn close_all(&mut self) {
        for (_, channel) in self.open.drain() {
            channel.ended.cancel();
        }
    }

    async fn refuse(&self, channel_id: u64, message: String) {
        let _ = self
            .outbound
            .send(closed(channel_id, None, Some(message)))
            .await;
    }

    async fn start(
        &self,
        channel_id: u64,
        kind: GuestChannelKind,
        context: Option<&ShellContext>,
    ) -> Result<OpenChannel, String> {
        match (kind, context) {
            (GuestChannelKind::Forward { port }, _) => self.start_forward(channel_id, port).await,
            (GuestChannelKind::Console, _) => {
                Err("the console is served by the worker, not the guest agent".to_owned())
            }
            (GuestChannelKind::Shell { .. }, None) => Err("the job has not started yet".to_owned()),
            (GuestChannelKind::Shell { attach: true, .. }, Some(context)) => {
                match &context.program {
                    Some(program) => self.start_attached(channel_id, program),
                    None => Err(NO_PROGRAM_TERMINAL.to_owned()),
                }
            }
            (GuestChannelKind::Shell { pty, attach: false }, Some(context)) => {
                self.start_shell(channel_id, context, pty)
            }
        }
    }

    fn start_shell(
        &self,
        channel_id: u64,
        context: &ShellContext,
        pty: bool,
    ) -> Result<OpenChannel, String> {
        let Shell {
            child,
            input,
            output,
            terminal,
        } = spawn_shell(context, pty).map_err(|e| format!("cannot start a shell: {e}"))?;
        let channel = self.channel_with_input(input, terminal);
        self.tasks.spawn(shell_output(
            channel_id,
            child,
            output,
            context.redactor.stream(),
            self.outbound.clone(),
            channel.ended.clone(),
        ));
        Ok(channel)
    }

    fn start_attached(
        &self,
        channel_id: u64,
        program: &ProgramTerminal,
    ) -> Result<OpenChannel, String> {
        let master = program.master();
        let input = master
            .stream()
            .map_err(|e| format!("cannot open the program terminal: {e}"))?;
        let channel = self.channel_with_input(Box::new(input), Some(master));
        self.tasks.spawn(attached_output(
            channel_id,
            program.follow(),
            program.clone(),
            self.outbound.clone(),
            channel.ended.clone(),
        ));
        Ok(channel)
    }

    async fn start_forward(&self, channel_id: u64, port: u16) -> Result<OpenChannel, String> {
        let stream = TcpStream::connect(("127.0.0.1", port))
            .await
            .map_err(|_| format!("nothing listens on port {port} in the guest"))?;
        let (reader, writer) = stream.into_split();
        let channel = self.channel_with_input(Box::new(writer), None);
        self.tasks.spawn(forward_output(
            channel_id,
            reader,
            self.outbound.clone(),
            channel.ended.clone(),
        ));
        Ok(channel)
    }

    fn channel_with_input(&self, input: Input, terminal: Option<Arc<PtyMaster>>) -> OpenChannel {
        let (sender, queue) = mpsc::channel(INPUT_QUEUE);
        let ended = CancellationToken::new();
        self.tasks.spawn(write_input(queue, input, ended.clone()));
        OpenChannel {
            input: sender,
            terminal,
            ended,
        }
    }
}

impl Drop for AgentChannels {
    fn drop(&mut self) {
        self.close_all();
    }
}

fn closed(channel_id: u64, exit_code: Option<i32>, message: Option<String>) -> AgentToWorker {
    AgentToWorker::CloseChannel {
        channel_id,
        exit_code,
        message,
    }
}

async fn write_input(
    mut queue: mpsc::Receiver<Vec<u8>>,
    mut input: Input,
    ended: CancellationToken,
) {
    loop {
        let data = tokio::select! {
            () = ended.cancelled() => return,
            data = queue.recv() => data,
        };
        let Some(data) = data else {
            return;
        };
        let written = tokio::select! {
            () = ended.cancelled() => return,
            written = write_and_flush(&mut input, &data) => written,
        };
        if let Err(e) = written {
            tracing::debug!(error = %e, "cannot write channel input");
            return;
        }
    }
}

async fn write_and_flush(input: &mut Input, data: &[u8]) -> std::io::Result<()> {
    input.write_all(data).await?;
    input.flush().await
}

async fn send_output(
    channel_id: u64,
    mut output: impl AsyncRead + Unpin,
    mut redactor: Option<StreamRedactor>,
    outbound: &mpsc::Sender<AgentToWorker>,
) {
    let mut buf = vec![0; GUEST_CHANNEL_CHUNK];
    while let Ok(n) = output.read(&mut buf).await
        && n > 0
    {
        let data = match redactor.as_mut() {
            Some(redactor) => redactor.push(&buf[..n]),
            None => buf[..n].to_vec(),
        };
        if !send_data(channel_id, &data, outbound).await {
            return;
        }
    }
    if let Some(mut redactor) = redactor {
        send_data(channel_id, &redactor.finish(), outbound).await;
    }
}

async fn send_data(channel_id: u64, data: &[u8], outbound: &mpsc::Sender<AgentToWorker>) -> bool {
    for piece in data.chunks(GUEST_CHANNEL_CHUNK) {
        let data = piece.to_vec();
        if outbound
            .send(AgentToWorker::ChannelData { channel_id, data })
            .await
            .is_err()
        {
            return false;
        }
    }
    true
}

async fn shell_output(
    channel_id: u64,
    mut child: Child,
    output: Box<dyn AsyncRead + Send + Unpin>,
    redactor: StreamRedactor,
    outbound: mpsc::Sender<AgentToWorker>,
    ended: CancellationToken,
) {
    let group = child
        .id()
        .and_then(|id| i32::try_from(id).ok())
        .map(Pid::from_raw);
    let status = {
        let finished = async {
            let pump = send_output(channel_id, output, Some(redactor), &outbound);
            let exit = child.wait();
            tokio::pin!(pump, exit);
            tokio::select! {
                () = &mut pump => exit.await,
                status = &mut exit => {
                    let _ = tokio::time::timeout(OUTPUT_DRAIN, pump).await;
                    status
                }
            }
        };
        tokio::select! {
            status = finished => Some(status),
            () = ended.cancelled() => None,
        }
    };
    match status {
        Some(status) => {
            let _ = outbound
                .send(closed(channel_id, Some(exit_code(status)), None))
                .await;
            ended.cancel();
        }
        None => hang_up(group, &mut child).await,
    }
}

async fn hang_up(group: Option<Pid>, child: &mut Child) {
    signal_group(group, Signal::SIGHUP);
    if tokio::time::timeout(HANG_UP_GRACE, child.wait())
        .await
        .is_err()
    {
        signal_group(group, Signal::SIGKILL);
        let _ = child.wait().await;
    }
}

async fn forward_output(
    channel_id: u64,
    reader: tokio::net::tcp::OwnedReadHalf,
    outbound: mpsc::Sender<AgentToWorker>,
    ended: CancellationToken,
) {
    tokio::select! {
        () = send_output(channel_id, reader, None, &outbound) => {
            let _ = outbound.send(closed(channel_id, None, None)).await;
            ended.cancel();
        }
        () = ended.cancelled() => {}
    }
}

async fn attached_output(
    channel_id: u64,
    reader: OutputReader,
    program: ProgramTerminal,
    outbound: mpsc::Sender<AgentToWorker>,
    ended: CancellationToken,
) {
    tokio::select! {
        () = send_followed(channel_id, reader, &outbound) => {
            let _ = outbound.send(closed(channel_id, program.exit_code(), None)).await;
            ended.cancel();
        }
        () = ended.cancelled() => {}
    }
}

async fn send_followed(
    channel_id: u64,
    mut reader: OutputReader,
    outbound: &mpsc::Sender<AgentToWorker>,
) {
    while let Some(chunk) = reader.next().await {
        let OutputChunk::Data(data) = chunk else {
            continue;
        };
        if !send_data(channel_id, &data, outbound).await {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::OwnedFd;

    use tokio::net::TcpListener;
    use tokio::sync::watch;

    use super::*;
    use crate::output::OutputLog;
    use crate::shell::ShellContext;

    struct ProgramFixture {
        context: ShellContext,
        log: OutputLog,
        slave: OwnedFd,
        exit: watch::Sender<Option<i32>>,
        _dir: tempfile::TempDir,
    }

    async fn program_fixture() -> ProgramFixture {
        let dir = tempfile::tempdir().unwrap();
        let log = OutputLog::create(&dir.path().join("output.log"), 1024 * 1024)
            .await
            .unwrap();
        let (master, slave) = PtyMaster::open().unwrap();
        let (exit, exit_watch) = watch::channel(None);
        let context = ShellContext {
            program: Some(ProgramTerminal::new(master, log.readers(), exit_watch)),
            ..ShellContext::default()
        };
        ProgramFixture {
            context,
            log,
            slave,
            exit,
            _dir: dir,
        }
    }

    fn attach() -> GuestChannelKind {
        GuestChannelKind::Shell {
            pty: false,
            attach: true,
        }
    }

    #[tokio::test]
    async fn an_attach_without_a_program_terminal_is_refused() {
        let (mut channels, mut sent) = channels();

        channels
            .open(
                8,
                GuestChannelKind::Shell {
                    pty: true,
                    attach: true,
                },
                Some(&ShellContext::default()),
            )
            .await;

        assert_eq!(
            sent.recv().await,
            Some(closed(
                8,
                None,
                Some("program has no terminal, set ssh = true".into())
            ))
        );
    }

    #[tokio::test]
    async fn an_attached_channel_follows_the_program_and_types_into_its_terminal() {
        let mut program = program_fixture().await;
        let (mut channels, mut sent) = channels();

        channels.open(9, attach(), Some(&program.context)).await;
        program.log.append(b"$ ").await.unwrap();
        assert_eq!(
            sent.recv().await,
            Some(AgentToWorker::ChannelData {
                channel_id: 9,
                data: b"$ ".to_vec()
            })
        );
        channels.data(9, b"echo ok\n".to_vec()).await;
        let mut typed = tokio::fs::File::from_std(program.slave.try_clone().unwrap().into());
        let mut line = vec![0; 8];
        typed.read_exact(&mut line).await.unwrap();
        assert_eq!(line, b"echo ok\n");

        channels.close(9);
        let mut terminal = program
            .context
            .program
            .as_ref()
            .unwrap()
            .master()
            .stream()
            .unwrap();
        terminal.write_all(b"still here\n").await.unwrap();
        terminal.flush().await.unwrap();
    }

    #[tokio::test]
    async fn an_attached_channel_closes_with_the_program_exit_code() {
        let program = program_fixture().await;
        let (mut channels, mut sent) = channels();

        channels.open(9, attach(), Some(&program.context)).await;
        program.exit.send_replace(Some(0));
        program.log.close();

        assert_eq!(sent.recv().await, Some(closed(9, Some(0), None)));
    }

    fn channels() -> (AgentChannels, mpsc::Receiver<AgentToWorker>) {
        let (outbound, sent) = mpsc::channel(16);
        (AgentChannels::new(outbound, Shutdown::new()), sent)
    }

    #[tokio::test]
    async fn bytes_cross_a_forward_both_ways_and_a_worker_close_ends_it() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let echo = Shutdown::new().spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 4];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(&buf).await.unwrap();
            stream.read(&mut buf).await.unwrap()
        });
        let (mut channels, mut sent) = channels();

        channels
            .open(5, GuestChannelKind::Forward { port }, None)
            .await;
        channels.data(5, b"ping".to_vec()).await;
        assert_eq!(
            sent.recv().await,
            Some(AgentToWorker::ChannelData {
                channel_id: 5,
                data: b"ping".to_vec()
            })
        );
        channels.close(5);

        assert_eq!(echo.await.unwrap(), 0);
    }

    #[tokio::test]
    async fn a_forward_to_a_closed_port_closes_with_a_message() {
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let (mut channels, mut sent) = channels();

        channels
            .open(6, GuestChannelKind::Forward { port }, None)
            .await;

        assert_eq!(
            sent.recv().await,
            Some(closed(
                6,
                None,
                Some(format!("nothing listens on port {port} in the guest"))
            ))
        );
    }

    #[tokio::test]
    async fn a_shell_before_the_job_is_refused() {
        let (mut channels, mut sent) = channels();

        channels
            .open(
                7,
                GuestChannelKind::Shell {
                    pty: true,
                    attach: false,
                },
                None,
            )
            .await;

        assert!(matches!(
            sent.recv().await,
            Some(AgentToWorker::CloseChannel {
                channel_id: 7,
                exit_code: None,
                message: Some(_),
            })
        ));
        drop(channels);
        assert_eq!(sent.recv().await, None);
    }
}
