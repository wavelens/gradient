/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::future::Future;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use gradient_redact::SecretRedactor;
use gradient_types::secret::SecretString;
use gradient_vm::{
    DeviceHold, GUEST_BOOT_TIMEOUT, Guest, MountConfig, PlugPlan, PluggedDevices, WORK_MOUNT_TAG,
    guest_address, wait_ready,
};
use gradient_wire::guest::{AgentToWorker, GuestMount, GuestNetwork, GuestSecret, RunJob};
use gradient_wire::types::{ExecJob, MAX_PHASE_PAYLOAD};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use super::agent_client::AgentClient;
use super::channels::GuestChannelMux;
use super::checkout::checkout_work;
use super::context::context_layers;
use super::host::ExecHost;
use super::inventory::device_requests;
use super::job_channels::{GuestChannelInbox, JobChannels};
use super::sops::WorkerSecrets;
use super::state::StoredJob;
use super::watchdog::{Watchdog, Watched};
use crate::executor::AbortSignal;
use crate::executor::failure::{BuildError, JobAborted};
use crate::nix::store::LocalNixStore;
use crate::proto::credentials::CredentialStore;
use crate::proto::job::JobUpdater;

const STORE_DIR: &str = "/nix/store/";
const KILL_GRACE: Duration = Duration::from_secs(15);

#[async_trait]
pub trait ExecReport: Send {
    async fn program_started(&mut self) -> Result<()>;
    async fn output(&mut self, data: Vec<u8>) -> Result<()>;
    async fn output_dropped(&mut self, bytes: u64) -> Result<()>;
    async fn phase(&mut self, name: String, payload: Vec<u8>) -> Result<()>;
    async fn note(&mut self, line: &str) -> Result<()>;
    async fn channel_data(&mut self, channel_id: u64, data: Vec<u8>);
    async fn channel_closed(
        &mut self,
        channel_id: u64,
        exit_code: Option<i32>,
        message: Option<String>,
    );
}

#[async_trait]
impl ExecReport for JobUpdater {
    async fn program_started(&mut self) -> Result<()> {
        self.report_program_started().await
    }

    async fn output(&mut self, data: Vec<u8>) -> Result<()> {
        JobUpdater::send_log_chunk(self, data).await
    }

    async fn output_dropped(&mut self, bytes: u64) -> Result<()> {
        self.skip_log_bytes(bytes).await?;
        self.send_log_note(&format!("{bytes} bytes of output dropped in the guest"))
            .await
    }

    async fn phase(&mut self, name: String, payload: Vec<u8>) -> Result<()> {
        self.send_job_phase(name, payload).await
    }

    async fn note(&mut self, line: &str) -> Result<()> {
        self.send_log_note(line).await
    }

    async fn channel_data(&mut self, channel_id: u64, data: Vec<u8>) {
        if let Err(e) = self.send_guest_channel_data(channel_id, data).await {
            debug!(channel_id, error = %e, "guest channel output dropped");
        }
    }

    async fn channel_closed(
        &mut self,
        channel_id: u64,
        exit_code: Option<i32>,
        message: Option<String>,
    ) {
        if let Err(e) = self
            .send_guest_channel_closed(channel_id, exit_code, message)
            .await
        {
            debug!(channel_id, error = %e, "guest channel close dropped");
        }
    }
}

#[derive(Debug)]
pub enum Followed {
    Exited(i32),
    Ended(BuildError),
    Stopped(Stop),
    LeftRunning,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stop {
    Silent(Duration),
    Total(Duration),
    Aborted,
}

impl Stop {
    fn log_line(self) -> Option<String> {
        match self {
            Stop::Silent(after) => Some(format!(
                "program produced no output for {}s (maxSilent exceeded)",
                after.as_secs()
            )),
            Stop::Total(after) => Some(format!(
                "program exceeded wall-clock timeout of {}s",
                after.as_secs()
            )),
            Stop::Aborted => None,
        }
    }

    fn into_error(self) -> anyhow::Error {
        match self.log_line() {
            Some(line) => BuildError::timeout(anyhow!(line)).into(),
            None => JobAborted("exec job aborted by server".to_owned()).into(),
        }
    }
}

pub async fn execute(
    host: &ExecHost,
    store: &LocalNixStore,
    mut job: ExecJob,
    updater: &mut JobUpdater,
    credentials: &CredentialStore,
    abort: AbortSignal,
    channels: GuestChannelInbox,
) -> Result<()> {
    let token = SecretString::new(std::mem::take(&mut job.context.token));
    let secrets = job_secrets(
        &updater.job_id,
        &job.secrets,
        credentials,
        &host.secrets,
        &host.worker_id,
    )?;
    let redactor = job_redactor(&secrets, token.expose());
    let mut run = ExecRun {
        host,
        job,
        token,
        secrets,
        redactor,
        updater,
        abort,
    };
    run.ensure_program(store).await?;
    let devices = run.reserve_devices()?;
    let mut guest = run.take_guest().await?;
    let outcome = run
        .run_in_guest(&mut guest, &devices, credentials, channels)
        .await;
    host.pool.retire(guest, devices);
    outcome
}

pub(crate) fn job_secrets(
    job_id: &str,
    names: &[String],
    credentials: &CredentialStore,
    worker: &WorkerSecrets,
    worker_id: &str,
) -> Result<Vec<GuestSecret>, BuildError> {
    names
        .iter()
        .map(|name| {
            let value = match (credentials.job_secret(job_id, name), worker.value(name)) {
                (Some(value), None) | (None, Some(value)) => value,
                (Some(_), Some(_)) => {
                    return Err(BuildError::permanent(anyhow!(
                        "secret \"{name}\" found in two sources: server, worker sops of {worker_id}"
                    )));
                }
                (None, None) => {
                    return Err(BuildError::permanent(anyhow!(
                        "secret \"{name}\" not available on this worker"
                    )));
                }
            };
            Ok(GuestSecret {
                name: name.clone(),
                value,
            })
        })
        .collect()
}

pub(crate) fn job_redactor(secrets: &[GuestSecret], token: &str) -> SecretRedactor {
    let token = (!token.is_empty()).then_some(("job-token", token.as_bytes()));
    SecretRedactor::new(
        secrets
            .iter()
            .map(|secret| (secret.name.as_str(), secret.value.expose()))
            .chain(token),
    )
}

pub async fn finish<R: ExecReport>(
    agent: &mut AgentClient,
    reporter: &mut R,
    watchdog: &mut Watchdog,
    abort: &mut AbortSignal,
    leave_running: &CancellationToken,
    channels: &mut JobChannels,
) -> Result<()> {
    let followed = follow(agent, reporter, watchdog, abort, leave_running, channels).await;
    if !matches!(followed, Followed::LeftRunning) {
        let exit_code = match followed {
            Followed::Exited(code) => Some(code),
            _ => None,
        };
        channels.end(exit_code, agent, reporter).await;
    }
    let stop = match followed {
        Followed::Exited(code) => return exit_result(code).map_err(anyhow::Error::from),
        Followed::Ended(e) => return Err(e.into()),
        Followed::LeftRunning => {
            info!("the worker stops and leaves the exec job running in its guest");
            return std::future::pending().await;
        }
        Followed::Stopped(stop) => stop,
    };
    if let Some(line) = stop.log_line()
        && let Err(e) = reporter.note(&line).await
    {
        warn!(error = %e, "could not log the timeout");
    }
    if let Err(e) = agent.kill().await {
        warn!(error = %e, "could not ask the guest agent to stop the program");
    }
    if stop != Stop::Aborted {
        drain_until_exit(agent, reporter).await;
    }
    Err(stop.into_error())
}

pub async fn follow<R: ExecReport>(
    agent: &mut AgentClient,
    reporter: &mut R,
    watchdog: &mut Watchdog,
    abort: &mut AbortSignal,
    leave_running: &CancellationToken,
    channels: &mut JobChannels,
) -> Followed {
    loop {
        let watched = tokio::select! {
            () = leave_running.cancelled() => return Followed::LeftRunning,
            event = channels.next_event() => {
                channels.handle(event, agent, reporter).await;
                continue;
            }
            watched = async { watchdog.next(&mut agent.messages(), abort).await } => watched,
        };
        let message = match watched {
            Watched::Item(Ok(message)) => message,
            Watched::End => {
                return Followed::Ended(BuildError::transient(anyhow!(
                    "the guest agent closed the connection before the program exited"
                )));
            }
            Watched::Item(Err(e)) => {
                return Followed::Ended(BuildError::transient(e.context("lost the guest agent")));
            }
            Watched::Silent(after) => return Followed::Stopped(Stop::Silent(after)),
            Watched::TotalExceeded(after) => return Followed::Stopped(Stop::Total(after)),
            Watched::Aborted => return Followed::Stopped(Stop::Aborted),
        };
        let message = match message {
            AgentToWorker::ChannelData { channel_id, data } => {
                channels.agent_data(channel_id, data, agent, reporter).await;
                continue;
            }
            AgentToWorker::CloseChannel {
                channel_id,
                exit_code,
                message,
            } => {
                channels
                    .agent_closed(channel_id, exit_code, message, agent, reporter)
                    .await;
                continue;
            }
            AgentToWorker::JobSecrets { secrets, token } => {
                channels.mask_console(&job_redactor(&secrets, &token));
                continue;
            }
            message => message,
        };
        if matches!(
            message,
            AgentToWorker::Output { .. } | AgentToWorker::OutputDropped { .. }
        ) {
            watchdog.saw_output();
        }
        match forward(message, reporter).await {
            Ok(None) => {}
            Ok(Some(end)) => return end,
            Err(e) => return Followed::Ended(BuildError::transient(e)),
        }
    }
}

pub fn exit_result(code: i32) -> Result<(), BuildError> {
    match code {
        0 => Ok(()),
        code => Err(BuildError::permanent(anyhow!(
            "program exited with code {code}"
        ))),
    }
}

async fn forward<R: ExecReport>(
    message: AgentToWorker,
    reporter: &mut R,
) -> Result<Option<Followed>> {
    match message {
        AgentToWorker::Ready { agent_version } => {
            debug!(%agent_version, "guest agent sent Ready again");
        }
        AgentToWorker::ProgramStarted => reporter.program_started().await?,
        AgentToWorker::Output { data } => reporter.output(data).await?,
        AgentToWorker::OutputDropped { bytes } => reporter.output_dropped(bytes).await?,
        AgentToWorker::Phase { name, payload } if payload.len() > MAX_PHASE_PAYLOAD => {
            warn!(%name, bytes = payload.len(), "phase payload above the limit dropped");
        }
        AgentToWorker::Phase { name, payload } => reporter.phase(name, payload).await?,
        AgentToWorker::SetupFailed { message } => {
            return Ok(Some(Followed::Ended(BuildError::permanent(anyhow!(
                message
            )))));
        }
        AgentToWorker::JobLost => {
            return Ok(Some(Followed::Ended(BuildError::transient(anyhow!(
                "the guest agent restarted and lost the job"
            )))));
        }
        AgentToWorker::Exited { code } => return Ok(Some(Followed::Exited(code))),
        AgentToWorker::ChannelData { channel_id, .. }
        | AgentToWorker::CloseChannel { channel_id, .. } => {
            debug!(channel_id, "guest channel frame dropped");
        }
        AgentToWorker::JobSecrets { .. } => debug!("job secrets from the guest agent dropped"),
    }
    Ok(None)
}

async fn drain_until_exit<R: ExecReport>(agent: &mut AgentClient, reporter: &mut R) {
    let drain = async {
        while let Ok(Some(message)) = agent.next().await {
            if !matches!(forward(message, reporter).await, Ok(None)) {
                return;
            }
        }
    };
    if tokio::time::timeout(KILL_GRACE, drain).await.is_err() {
        warn!(
            "the program did not exit within {}s of the kill",
            KILL_GRACE.as_secs()
        );
    }
}

struct ExecRun<'a> {
    host: &'a ExecHost,
    job: ExecJob,
    token: SecretString,
    secrets: Vec<GuestSecret>,
    redactor: SecretRedactor,
    updater: &'a mut JobUpdater,
    abort: AbortSignal,
}

impl ExecRun<'_> {
    async fn ensure_program(&mut self, store: &LocalNixStore) -> Result<()> {
        if self.job.image.is_some() {
            return Err(BuildError::permanent(anyhow!(
                "this worker boots only its own guest image, custom images are not supported yet"
            ))
            .into());
        }
        let program = program_store_path(&self.job.program)?.to_owned();
        let abort = self.abort.clone();
        let fetch = crate::proto::prefetch::ensure_path(store, &program, self.updater);
        unless_aborted(&abort, fetch)
            .await?
            .map_err(BuildError::transient)?;
        Ok(())
    }

    fn reserve_devices(&self) -> Result<DeviceHold> {
        let requests = device_requests(&self.job.requirement.devices);
        self.host
            .devices
            .reserve(&requests)
            .ok_or_else(|| BuildError::transient(anyhow!("devices no longer free")).into())
    }

    async fn take_guest(&self) -> Result<Guest> {
        let guest = unless_aborted(
            &self.abort,
            self.host.pool.take(self.updater.job_id.clone()),
        )
        .await?;
        Ok(guest.map_err(BuildError::guest_boot)?)
    }

    async fn run_in_guest(
        &mut self,
        guest: &mut Guest,
        devices: &DeviceHold,
        credentials: &CredentialStore,
        inbox: GuestChannelInbox,
    ) -> Result<()> {
        let abort = self.abort.clone();
        let mut agent =
            unless_aborted(&abort, self.start_program(guest, devices, credentials)).await??;
        let requirement = &self.job.requirement;
        let mut mux =
            GuestChannelMux::new(requirement.ssh, requirement.ports.clone(), agent.version());
        mux.mask_console(&self.redactor);
        let mut channels = JobChannels::new(inbox, mux, guest.dir().to_owned());
        let mut watchdog = Watchdog::new(
            Instant::now(),
            self.job.timeout_secs.map(Duration::from_secs),
            self.job.max_silent_secs.map(Duration::from_secs),
        );
        finish(
            &mut agent,
            &mut *self.updater,
            &mut watchdog,
            &mut self.abort,
            &self.host.worker_stop_keeping_guests(),
            &mut channels,
        )
        .await
    }

    async fn start_program(
        &mut self,
        guest: &mut Guest,
        devices: &DeviceHold,
        credentials: &CredentialStore,
    ) -> Result<AgentClient> {
        guest
            .record_job(&self.stored_job(false))
            .map_err(BuildError::transient)?;
        let work = guest.dir().join("work");
        std::fs::create_dir_all(&work)
            .with_context(|| format!("create {}", work.display()))
            .map_err(BuildError::transient)?;
        checkout_work(
            &work,
            &self.job.context,
            credentials,
            &mut self.abort.clone(),
        )
        .await
        .map_err(BuildError::transient)?;
        guest.resume().await.map_err(BuildError::transient)?;

        let mounts = self.mounts(guest)?;
        let network = self.network(guest)?;
        let plan = PlugPlan {
            units: devices.units(),
            mounts: mounts.clone(),
            work,
            network: network.clone(),
        };
        let plugged = guest.plug(&plan).await.map_err(BuildError::transient)?;
        let run_job = self.run_job(&plugged, &mounts, network);

        let ready = wait_ready(&guest.vsock_socket(), GUEST_BOOT_TIMEOUT)
            .await
            .map_err(BuildError::guest_boot)?;
        let mut agent = AgentClient::from(ready);
        info!(
            agent_version = agent.agent_version(),
            "guest agent ready for the job"
        );
        agent.attach(0).await.map_err(BuildError::transient)?;
        agent.run(run_job).await.map_err(BuildError::transient)?;
        guest
            .record_job(&self.stored_job(true))
            .map_err(BuildError::transient)?;
        Ok(agent)
    }

    fn mounts(&self, guest: &Guest) -> Result<Vec<(MountConfig, PathBuf)>, BuildError> {
        self.job
            .requirement
            .mounts
            .iter()
            .map(|name| {
                let mount = self
                    .host
                    .config
                    .mounts
                    .iter()
                    .find(|mount| &mount.name == name)
                    .ok_or_else(|| {
                        BuildError::permanent(anyhow!(
                            "mount {name} is not configured on this worker"
                        ))
                    })?;
                let dir = match mount.per_job {
                    true => mount.path.join(guest.id().to_string()),
                    false => mount.path.clone(),
                };
                Ok((mount.clone(), dir))
            })
            .collect()
    }

    fn network(&self, guest: &Guest) -> Result<Option<GuestNetwork>, BuildError> {
        if !self.job.requirement.network {
            return Ok(None);
        }
        guest_address(
            &self.host.pool.config().subnet,
            guest.state().cid,
            &self.host.config.nameservers,
        )
        .map(Some)
        .map_err(BuildError::permanent)
    }

    fn run_job(
        &self,
        plugged: &PluggedDevices,
        mounts: &[(MountConfig, PathBuf)],
        network: Option<GuestNetwork>,
    ) -> RunJob {
        let names: Vec<String> = mounts.iter().map(|(mount, _)| mount.name.clone()).collect();
        let context = context_layers(
            &self.job,
            &self.token,
            &self.host.worker_id,
            self.host.zone.as_deref(),
            plugged,
            &names,
            network.as_ref().map(|network| network.address.as_str()),
        );
        RunJob {
            program: self.job.program.clone(),
            args: self.job.args.clone(),
            env: self.job.env.clone(),
            context,
            last_phase_payload: self
                .job
                .context
                .last_phase
                .as_ref()
                .map(|phase| phase.payload.clone())
                .unwrap_or_default(),
            secrets: self.secrets.clone(),
            mounts: guest_mounts(mounts),
            groups: plugged.groups.clone(),
            network,
            relays: plugged.relays.clone(),
            usb_bus_ids: plugged.usb_bus_ids.clone(),
            program_pty: self.job.requirement.ssh,
        }
    }

    fn stored_job(&self, sent_to_agent: bool) -> StoredJob {
        StoredJob {
            job_id: self.updater.job_id.clone(),
            assignment_id: self.updater.assignment_id.get(),
            attempt: self.job.attempt,
            timeout_secs: self.job.timeout_secs,
            max_silent_secs: self.job.max_silent_secs,
            started_at_unix: unix_now(),
            sent_to_agent,
            ssh: self.job.requirement.ssh,
            ports: self.job.requirement.ports.clone(),
        }
    }
}

fn guest_mounts(mounts: &[(MountConfig, PathBuf)]) -> Vec<GuestMount> {
    let work = GuestMount {
        tag: WORK_MOUNT_TAG.to_owned(),
        path: "/work".to_owned(),
        read_only: false,
    };
    let configured = mounts.iter().map(|(mount, _)| GuestMount {
        tag: mount.name.clone(),
        path: format!("/mnt/{}", mount.name),
        read_only: mount.read_only,
    });
    std::iter::once(work).chain(configured).collect()
}

fn program_store_path(program: &str) -> Result<&str, BuildError> {
    let name = program
        .strip_prefix(STORE_DIR)
        .and_then(|rest| rest.split('/').next())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| BuildError::permanent(anyhow!("program {program} is not in /nix/store")))?;
    Ok(&program[..STORE_DIR.len() + name.len()])
}

pub async fn unless_aborted<T>(abort: &AbortSignal, work: impl Future<Output = T>) -> Result<T> {
    let mut abort = abort.clone();
    tokio::select! {
        biased;
        () = abort.aborted() => Err(Stop::Aborted.into_error()),
        done = work => Ok(done),
    }
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use gradient_vm::ReadyAgent;
    use gradient_wire::guest::{
        AgentToWorker, GUEST_CHANNELS_AGENT_SINCE, GuestConnection, WorkerToAgent,
    };
    use gradient_wire::messages::{BuildFailureKind, CredentialKind, GuestChannelKind};
    use tokio::net::UnixStream;
    use tokio::sync::mpsc;

    use crate::exec::channels::GuestChannelCommand;

    use super::*;

    #[derive(Debug, PartialEq)]
    enum Reported {
        Started,
        Output(Vec<u8>),
        Dropped(u64),
        Phase(String),
        Note(String),
        ChannelData(u64, Vec<u8>),
        ChannelClosed(u64, Option<i32>),
    }

    #[derive(Default)]
    struct Recorder(Vec<Reported>);

    #[async_trait]
    impl ExecReport for Recorder {
        async fn program_started(&mut self) -> Result<()> {
            self.0.push(Reported::Started);
            Ok(())
        }

        async fn output(&mut self, data: Vec<u8>) -> Result<()> {
            self.0.push(Reported::Output(data));
            Ok(())
        }

        async fn output_dropped(&mut self, bytes: u64) -> Result<()> {
            self.0.push(Reported::Dropped(bytes));
            Ok(())
        }

        async fn phase(&mut self, name: String, _payload: Vec<u8>) -> Result<()> {
            self.0.push(Reported::Phase(name));
            Ok(())
        }

        async fn note(&mut self, line: &str) -> Result<()> {
            self.0.push(Reported::Note(line.to_owned()));
            Ok(())
        }

        async fn channel_data(&mut self, channel_id: u64, data: Vec<u8>) {
            self.0.push(Reported::ChannelData(channel_id, data));
        }

        async fn channel_closed(
            &mut self,
            channel_id: u64,
            exit_code: Option<i32>,
            _message: Option<String>,
        ) {
            self.0.push(Reported::ChannelClosed(channel_id, exit_code));
        }
    }

    fn job_channels(ssh: bool) -> (mpsc::Sender<GuestChannelCommand>, JobChannels) {
        let (commands, inbox) = mpsc::channel(8);
        let mux = GuestChannelMux::new(ssh, Vec::new(), GUEST_CHANNELS_AGENT_SINCE);
        (commands, JobChannels::new(inbox, mux, PathBuf::new()))
    }

    fn no_channels() -> JobChannels {
        job_channels(false).1
    }

    async fn connected() -> (AgentClient, GuestConnection<UnixStream>) {
        let (worker_end, agent_end) = UnixStream::pair().unwrap();
        let (worker, agent) = tokio::join!(
            GuestConnection::agree(worker_end),
            GuestConnection::agree(agent_end)
        );
        let client = AgentClient::from(ReadyAgent {
            conn: worker.unwrap(),
            agent_version: "test".into(),
        });
        (client, agent.unwrap())
    }

    async fn followed(script: Vec<AgentToWorker>) -> (Followed, Vec<Reported>) {
        let (mut agent, mut guest) = connected().await;
        let fake_agent = async move {
            for message in script {
                guest.send(&message).await.unwrap();
            }
        };
        let mut recorder = Recorder::default();
        let mut watchdog = Watchdog::new(Instant::now(), None, None);
        let (mut abort, running) = (AbortSignal::never(), CancellationToken::new());
        let mut channels = no_channels();
        let (followed, ()) = tokio::join!(
            follow(
                &mut agent,
                &mut recorder,
                &mut watchdog,
                &mut abort,
                &running,
                &mut channels,
            ),
            fake_agent
        );
        (followed, recorder.0)
    }

    #[tokio::test]
    async fn a_silent_program_is_killed_and_its_last_output_kept() {
        let (mut agent, mut guest) = connected().await;
        let fake_agent = async move {
            guest.send(&AgentToWorker::ProgramStarted).await.unwrap();
            let kill = guest.recv::<WorkerToAgent>().await.unwrap();
            assert_eq!(kill, Some(WorkerToAgent::Kill));
            guest
                .send(&AgentToWorker::Output {
                    data: b"bye".to_vec(),
                })
                .await
                .unwrap();
            guest
                .send(&AgentToWorker::Exited { code: 143 })
                .await
                .unwrap();
            guest
        };
        let mut recorder = Recorder::default();
        let mut watchdog = Watchdog::new(Instant::now(), None, Some(Duration::from_secs(1)));
        let (mut abort, running) = (AbortSignal::never(), CancellationToken::new());
        let mut channels = no_channels();
        let finished = finish(
            &mut agent,
            &mut recorder,
            &mut watchdog,
            &mut abort,
            &running,
            &mut channels,
        );

        let (finished, _guest) = tokio::join!(finished, fake_agent);

        let error = finished.unwrap_err();
        assert_eq!(
            error.downcast_ref::<BuildError>().unwrap().kind,
            BuildFailureKind::Timeout
        );
        assert_eq!(
            recorder.0,
            [
                Reported::Started,
                Reported::Note("program produced no output for 1s (maxSilent exceeded)".into()),
                Reported::Output(b"bye".to_vec()),
            ]
        );
    }

    #[tokio::test]
    async fn a_worker_stop_keeping_guests_leaves_the_program_running() {
        let (mut agent, _guest) = connected().await;
        let stopping = CancellationToken::new();
        stopping.cancel();

        let followed = follow(
            &mut agent,
            &mut Recorder::default(),
            &mut Watchdog::new(Instant::now(), None, None),
            &mut AbortSignal::never(),
            &stopping,
            &mut no_channels(),
        )
        .await;

        assert!(matches!(followed, Followed::LeftRunning), "{followed:?}");
    }

    #[tokio::test(start_paused = true)]
    async fn channel_traffic_does_not_reset_max_silent() {
        let (mut agent, mut guest) = connected().await;
        let (commands, mut channels) = job_channels(true);
        let shell = GuestChannelKind::Shell {
            pty: true,
            attach: false,
        };
        commands
            .send(GuestChannelCommand::Open {
                channel_id: 4,
                kind: shell,
            })
            .await
            .unwrap();
        let fake_agent = async move {
            tokio::time::sleep(Duration::from_secs(9)).await;
            guest
                .send(&AgentToWorker::ChannelData {
                    channel_id: 4,
                    data: b"$ ".to_vec(),
                })
                .await
                .unwrap();
            guest
        };
        let mut recorder = Recorder::default();
        let started = Instant::now();
        let mut watchdog = Watchdog::new(started, None, Some(Duration::from_secs(10)));
        let (mut abort, running) = (AbortSignal::never(), CancellationToken::new());

        let (followed, _guest) = tokio::join!(
            follow(
                &mut agent,
                &mut recorder,
                &mut watchdog,
                &mut abort,
                &running,
                &mut channels,
            ),
            fake_agent
        );

        assert!(
            matches!(followed, Followed::Stopped(Stop::Silent(_))),
            "{followed:?}"
        );
        assert_eq!(started.elapsed(), Duration::from_secs(10));
        assert_eq!(recorder.0, [Reported::ChannelData(4, b"$ ".to_vec())]);
    }

    fn credential(name: &str, value: &[u8]) -> CredentialStore {
        let store = CredentialStore::new();
        store.store(
            CredentialKind::JobSecret {
                job_id: "exec:1".into(),
                name: name.into(),
            },
            value.into(),
        );
        store
    }

    fn worker_sops() -> std::sync::Arc<WorkerSecrets> {
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../gradient-sops/tests/fixtures");
        WorkerSecrets::open(
            &[fixtures.join("worker.yaml")],
            Some(&fixtures.join("ed25519")),
        )
        .unwrap()
    }

    fn names(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn secrets_come_from_the_credential_or_the_worker_sops() {
        let credentials = credential("wandb-token", b"s3cr3t-value");
        let secrets = job_secrets(
            "exec:1",
            &names(&["lab-key", "wandb-token"]),
            &credentials,
            &worker_sops(),
            "worker-7",
        )
        .unwrap();

        assert_eq!(
            secrets,
            [
                GuestSecret {
                    name: "lab-key".into(),
                    value: b"lab-value-123".to_vec().into()
                },
                GuestSecret {
                    name: "wandb-token".into(),
                    value: b"s3cr3t-value".to_vec().into()
                },
            ]
        );
    }

    #[test]
    fn a_name_in_the_credential_and_the_worker_sops_fails() {
        let credentials = credential("lab-key", b"other-value");
        let error = job_secrets(
            "exec:1",
            &names(&["lab-key"]),
            &credentials,
            &worker_sops(),
            "worker-7",
        )
        .unwrap_err();

        assert_eq!(error.kind, BuildFailureKind::Permanent);
        assert_eq!(
            error.to_string(),
            r#"secret "lab-key" found in two sources: server, worker sops of worker-7"#
        );
    }

    #[test]
    fn a_missing_secret_names_itself() {
        let error = job_secrets(
            "exec:1",
            &names(&["hf-token"]),
            &CredentialStore::new(),
            &worker_sops(),
            "worker-7",
        )
        .unwrap_err();

        assert_eq!(error.kind, BuildFailureKind::Permanent);
        assert_eq!(
            error.to_string(),
            r#"secret "hf-token" not available on this worker"#
        );
    }

    #[test]
    fn the_job_token_is_masked_as_job_token() {
        assert_eq!(
            job_redactor(&[], "gr_job_abcdefgh123").redact(b"t=gr_job_abcdefgh123"),
            b"t=[secret:job-token]"
        );
    }

    #[test]
    fn exit_codes_map_to_job_results() {
        assert!(exit_result(0).is_ok());
        let failed = exit_result(3).unwrap_err();
        assert_eq!(failed.kind, BuildFailureKind::Permanent);
        assert_eq!(failed.to_string(), "program exited with code 3");
    }

    #[tokio::test]
    async fn a_closed_agent_without_exit_is_a_lost_guest() {
        let (followed, reported) = followed(vec![
            AgentToWorker::ProgramStarted,
            AgentToWorker::Output {
                data: b"tick".to_vec(),
            },
        ])
        .await;

        assert!(
            matches!(&followed, Followed::Ended(e) if e.kind == BuildFailureKind::Transient),
            "{followed:?}"
        );
        assert_eq!(
            reported,
            [Reported::Started, Reported::Output(b"tick".to_vec())]
        );
    }

    #[tokio::test]
    async fn output_dropped_in_the_guest_moves_the_log_offset() {
        let (followed, reported) = followed(vec![
            AgentToWorker::OutputDropped { bytes: 4096 },
            AgentToWorker::Exited { code: 0 },
        ])
        .await;

        assert!(matches!(followed, Followed::Exited(0)), "{followed:?}");
        assert_eq!(reported, [Reported::Dropped(4096)]);
    }

    #[tokio::test]
    async fn a_phase_above_the_payload_limit_is_dropped() {
        let (_, reported) = followed(vec![
            AgentToWorker::Phase {
                name: "big".into(),
                payload: vec![0; MAX_PHASE_PAYLOAD + 1],
            },
            AgentToWorker::Phase {
                name: "small".into(),
                payload: vec![0; MAX_PHASE_PAYLOAD],
            },
            AgentToWorker::Exited { code: 0 },
        ])
        .await;

        assert_eq!(reported, [Reported::Phase("small".into())]);
    }

    #[test]
    fn a_program_runs_from_its_store_path() {
        assert_eq!(
            program_store_path("/nix/store/aaaa-train/bin/train").unwrap(),
            "/nix/store/aaaa-train"
        );
        assert!(program_store_path("/usr/bin/train").is_err());
        assert!(program_store_path("/nix/store/").is_err());
    }
}
