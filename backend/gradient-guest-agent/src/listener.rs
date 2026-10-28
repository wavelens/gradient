/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use gradient_util::shutdown::Shutdown;
use gradient_wire::guest::{
    AGENT_VSOCK_PORT, AgentToWorker, GUEST_JOB_SECRETS_SINCE, GuestConnection, GuestSender, RunJob,
    WorkerToAgent,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener, VsockStream};

use crate::channels::AgentChannels;
use crate::http::HttpState;
use crate::output::{OutputChunk, OutputReader, OutputReaders};
use crate::redaction::JobSecrets;
use crate::run::run;
use crate::setup::{PreparedJob, prepare};
use crate::shell::ShellContext;
use crate::terminal::ProgramTerminal;

const ACCEPT_RETRY: Duration = Duration::from_millis(100);
const SETUP_TIMEOUT: Duration = Duration::from_secs(300);

#[derive(Clone, Debug, Default)]
struct Progress {
    started: bool,
    outcome: Option<Outcome>,
}

#[derive(Clone, Debug)]
enum Outcome {
    Exited(i32),
    SetupFailed(String),
    Lost,
}

struct Agent {
    http: Arc<HttpState>,
    readers: OutputReaders,
    job_claimed: AtomicBool,
    progress: watch::Sender<Progress>,
    kill: CancellationToken,
    shell: Mutex<Option<ShellContext>>,
    job_secrets: Mutex<Option<JobSecrets>>,
    tasks: Shutdown,
}

impl Agent {
    fn shell_context(&self) -> Option<ShellContext> {
        self.shell
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    fn keep_shell_context(&self, context: ShellContext) {
        *self
            .shell
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(context);
    }

    fn job_secrets_message(&self) -> Option<AgentToWorker> {
        let secrets = self
            .job_secrets
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()?;
        Some(AgentToWorker::JobSecrets {
            secrets: secrets.secrets,
            token: secrets.token,
        })
    }

    fn keep_program_terminal(&self, terminal: ProgramTerminal) {
        if let Some(context) = self
            .shell
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_mut()
        {
            context.program = Some(terminal);
        }
    }
}

pub async fn listen(
    http: Arc<HttpState>,
    readers: OutputReaders,
    tasks: Shutdown,
    restarted: bool,
) -> Result<()> {
    let agent = Arc::new(Agent {
        http,
        readers,
        job_claimed: AtomicBool::new(restarted),
        progress: watch::Sender::new(initial_progress(restarted)),
        kill: CancellationToken::new(),
        shell: Mutex::new(None),
        job_secrets: Mutex::new(None),
        tasks,
    });
    let listener = VsockListener::bind(VsockAddr::new(VMADDR_CID_ANY, AGENT_VSOCK_PORT))
        .context("listen on the agent vsock port")?;
    tracing::info!(port = AGENT_VSOCK_PORT, "waiting for the worker");

    let mut current: Option<JoinHandle<()>> = None;
    loop {
        let (stream, peer) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(e) => {
                tracing::warn!(error = %e, "cannot accept a worker connection");
                tokio::time::sleep(ACCEPT_RETRY).await;
                continue;
            }
        };
        tracing::info!(%peer, "worker connected");
        if let Some(previous) = current.take() {
            previous.abort();
        }
        let connection_agent = Arc::clone(&agent);
        current = Some(agent.tasks.spawn(async move {
            if let Err(e) = serve_connection(connection_agent, stream).await {
                tracing::warn!(error = %format!("{e:#}"), "worker connection ended");
            }
        }));
    }
}

fn initial_progress(restarted: bool) -> Progress {
    Progress {
        started: false,
        outcome: restarted.then_some(Outcome::Lost),
    }
}

async fn prepare_until_stopped(job: &RunJob, agent: &Agent) -> Result<PreparedJob, String> {
    tokio::select! {
        prepared = prepare(job, &agent.tasks) => prepared,
        () = agent.kill.cancelled() => Err("killed during setup".to_owned()),
        () = tokio::time::sleep(SETUP_TIMEOUT) => Err(format!(
            "setup did not finish within {} s",
            SETUP_TIMEOUT.as_secs()
        )),
    }
}

async fn serve_connection(agent: Arc<Agent>, stream: VsockStream) -> Result<()> {
    let mut conn = GuestConnection::agree(stream).await?;
    let sends_job_secrets = conn.version() >= GUEST_JOB_SECRETS_SINCE;
    conn.send(&AgentToWorker::Ready {
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
    .await?;

    let version = conn.version();
    let (sender, mut receiver) = conn.split();
    let (outgoing, to_send) = mpsc::channel(16);
    let _writer = AbortOnDropHandle::new(agent.tasks.spawn(write_to_worker(sender, to_send)));
    let _linked = LinkedConnection {
        generation: agent.http.link.connect(outgoing.clone(), version),
        http: Arc::clone(&agent.http),
    };
    let mut channels = AgentChannels::new(outgoing.clone(), agent.tasks.clone());
    let mut _streamer = spawn_streamer(&agent, None, outgoing.clone());
    while let Some(message) = receiver.recv::<WorkerToAgent>().await? {
        match message {
            WorkerToAgent::Attach { output_offset } => {
                if let Some(secrets) = agent.job_secrets_message().filter(|_| sends_job_secrets) {
                    outgoing.send(secrets).await?;
                }
                let reader = agent.readers.reader_from(output_offset);
                _streamer = spawn_streamer(&agent, Some(reader), outgoing.clone());
            }
            WorkerToAgent::RunJob(job) => {
                if let Some(refused) = start_job(&agent, job) {
                    outgoing.send(refused).await?;
                }
            }
            WorkerToAgent::Kill => agent.kill.cancel(),
            WorkerToAgent::OpenChannel { channel_id, kind } => {
                let context = agent.shell_context();
                channels.open(channel_id, kind, context.as_ref()).await;
            }
            WorkerToAgent::ChannelData { channel_id, data } => {
                channels.data(channel_id, data).await;
            }
            WorkerToAgent::Resize {
                channel_id,
                cols,
                rows,
            } => channels.resize(channel_id, cols, rows),
            WorkerToAgent::CloseChannel { channel_id } => channels.close(channel_id),
            WorkerToAgent::UploadSettled { upload_id, error } => {
                agent.http.uploads.settled(upload_id, error);
            }
        }
    }
    Ok(())
}

struct LinkedConnection {
    generation: u64,
    http: Arc<HttpState>,
}

impl Drop for LinkedConnection {
    fn drop(&mut self) {
        self.http.link.disconnect(self.generation);
        self.http.uploads.connection_lost(self.generation);
    }
}

async fn write_to_worker(
    mut sender: GuestSender<VsockStream>,
    mut to_send: mpsc::Receiver<AgentToWorker>,
) {
    while let Some(message) = to_send.recv().await {
        if let Err(e) = sender.send(&message).await {
            tracing::warn!(error = %format!("{e:#}"), "cannot send to the worker");
            return;
        }
    }
}

fn start_job(agent: &Arc<Agent>, job: Box<RunJob>) -> Option<AgentToWorker> {
    if agent.job_claimed.swap(true, Ordering::SeqCst) {
        return Some(AgentToWorker::SetupFailed {
            message: "this guest already ran a job".to_owned(),
        });
    }
    let secrets = JobSecrets::of(&job);
    agent.http.hold_redactor(secrets.redactor());
    *agent
        .job_secrets
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(secrets);

    let job_agent = Arc::clone(agent);
    agent.tasks.spawn(async move {
        let agent = job_agent;
        let outcome = match prepare_until_stopped(&job, &agent).await {
            Ok(prepared) => {
                agent.keep_shell_context(ShellContext::of(
                    &prepared.program,
                    &job.groups,
                    prepared.redactor.clone(),
                ));
                let (started, started_rx) = oneshot::channel();
                let mark_started = async {
                    if let Ok(terminal) = started_rx.await {
                        if let Some(terminal) = terminal {
                            agent.keep_program_terminal(terminal);
                        }
                        agent.progress.send_modify(|p| p.started = true);
                    }
                };
                let (code, ()) = tokio::join!(
                    run(prepared, &agent.http, started, agent.kill.clone()),
                    mark_started
                );
                Outcome::Exited(code)
            }
            Err(message) => Outcome::SetupFailed(message),
        };
        tracing::info!(?outcome, "job ended");
        agent.progress.send_modify(|p| p.outcome = Some(outcome));
        agent.http.output.lock().await.close();
    });
    None
}

fn spawn_streamer(
    agent: &Agent,
    reader: Option<OutputReader>,
    outgoing: mpsc::Sender<AgentToWorker>,
) -> AbortOnDropHandle<()> {
    let progress = agent.progress.subscribe();
    AbortOnDropHandle::new(
        agent
            .tasks
            .spawn(stream_to_worker(progress, reader, outgoing)),
    )
}

async fn stream_to_worker(
    mut progress: watch::Receiver<Progress>,
    mut reader: Option<OutputReader>,
    outgoing: mpsc::Sender<AgentToWorker>,
) {
    let mut sent_started = false;
    let mut output_done = reader.is_none();
    loop {
        let now = progress.borrow_and_update().clone();
        match now.outcome.clone() {
            Some(Outcome::SetupFailed(message)) => {
                let _ = outgoing.send(AgentToWorker::SetupFailed { message }).await;
                return;
            }
            Some(Outcome::Lost) => {
                let _ = outgoing.send(AgentToWorker::JobLost).await;
                return;
            }
            Some(Outcome::Exited(_)) | None => {}
        }
        if reader.is_some() && now.started && !sent_started {
            sent_started = true;
            if outgoing.send(AgentToWorker::ProgramStarted).await.is_err() {
                return;
            }
        }
        if let (Some(_), true, Some(Outcome::Exited(code))) = (&reader, output_done, &now.outcome) {
            let _ = outgoing.send(AgentToWorker::Exited { code: *code }).await;
            return;
        }

        tokio::select! {
            chunk = next_chunk(reader.as_mut()), if !output_done => match chunk {
                Some(chunk) => {
                    if outgoing.send(chunk_message(chunk)).await.is_err() {
                        return;
                    }
                }
                None => output_done = true,
            },
            changed = progress.changed() => {
                if changed.is_err() {
                    return;
                }
            }
        }
    }
}

fn chunk_message(chunk: OutputChunk) -> AgentToWorker {
    match chunk {
        OutputChunk::Data(data) => AgentToWorker::Output { data },
        OutputChunk::Dropped(bytes) => AgentToWorker::OutputDropped { bytes },
    }
}

async fn next_chunk(reader: Option<&mut OutputReader>) -> Option<OutputChunk> {
    match reader {
        Some(reader) => reader.next().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
mod tests {
    use gradient_wire::guest::AgentToWorker;
    use tokio::sync::{mpsc, watch};

    use super::{initial_progress, stream_to_worker};

    #[tokio::test]
    async fn a_restarted_agent_reports_the_job_as_lost() {
        let (_progress, receiver) = watch::channel(initial_progress(true));
        let (outgoing, mut sent) = mpsc::channel(4);

        stream_to_worker(receiver, None, outgoing).await;

        assert_eq!(sent.recv().await, Some(AgentToWorker::JobLost));
        assert_eq!(sent.recv().await, None);
    }
}
