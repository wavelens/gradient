/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use gradient_util::shutdown::Shutdown;
use gradient_wire::guest::{
    AGENT_VSOCK_PORT, AgentToWorker, GuestConnection, RunJob, WorkerToAgent,
};
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tokio_util::task::AbortOnDropHandle;
use tokio_vsock::{VMADDR_CID_ANY, VsockAddr, VsockListener, VsockStream};

use crate::output::{OutputLog, OutputReader, OutputReaders};
use crate::run::run;
use crate::setup::{PreparedJob, prepare};

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
    readers: OutputReaders,
    unclaimed_output: Mutex<Option<OutputLog>>,
    progress: watch::Sender<Progress>,
    kill: CancellationToken,
    tasks: Shutdown,
}

pub async fn listen(output: OutputLog, tasks: Shutdown, restarted: bool) -> Result<()> {
    let agent = Arc::new(Agent {
        readers: output.readers(),
        unclaimed_output: Mutex::new((!restarted).then_some(output)),
        progress: watch::Sender::new(initial_progress(restarted)),
        kill: CancellationToken::new(),
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
    conn.send(&AgentToWorker::Ready {
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
    .await?;

    let (outgoing, mut to_send) = mpsc::channel(16);
    let mut _streamer = spawn_streamer(&agent, None, outgoing.clone());
    loop {
        tokio::select! {
            message = conn.recv::<WorkerToAgent>() => match message? {
                Some(WorkerToAgent::Attach { output_offset }) => {
                    let reader = agent.readers.reader_from(output_offset);
                    _streamer = spawn_streamer(&agent, Some(reader), outgoing.clone());
                }
                Some(WorkerToAgent::RunJob(job)) => {
                    if let Some(refused) = start_job(&agent, job) {
                        conn.send(&refused).await?;
                    }
                }
                Some(WorkerToAgent::Kill) => agent.kill.cancel(),
                None => return Ok(()),
            },
            Some(message) = to_send.recv() => conn.send(&message).await?,
        }
    }
}

fn start_job(agent: &Arc<Agent>, job: Box<RunJob>) -> Option<AgentToWorker> {
    let claimed = agent
        .unclaimed_output
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .take();
    let Some(mut output) = claimed else {
        return Some(AgentToWorker::SetupFailed {
            message: "this guest already ran a job".to_owned(),
        });
    };

    let job_agent = Arc::clone(agent);
    agent.tasks.spawn(async move {
        let agent = job_agent;
        let outcome = match prepare_until_stopped(&job, &agent).await {
            Ok(prepared) => {
                let (started, started_rx) = oneshot::channel();
                let mark_started = async {
                    if started_rx.await.is_ok() {
                        agent.progress.send_modify(|p| p.started = true);
                    }
                };
                let (code, ()) = tokio::join!(
                    run(prepared, &mut output, started, agent.kill.clone()),
                    mark_started
                );
                Outcome::Exited(code)
            }
            Err(message) => Outcome::SetupFailed(message),
        };
        tracing::info!(?outcome, "job ended");
        agent.progress.send_modify(|p| p.outcome = Some(outcome));
        output.close();
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
                Some(data) => {
                    if outgoing.send(AgentToWorker::Output { data }).await.is_err() {
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

async fn next_chunk(reader: Option<&mut OutputReader>) -> Option<Vec<u8>> {
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
