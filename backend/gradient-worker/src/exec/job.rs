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
use gradient_types::secret::SecretString;
use gradient_vm::{
    DeviceHold, GUEST_BOOT_TIMEOUT, Guest, MountConfig, PlugPlan, PluggedDevices, WORK_MOUNT_TAG,
    guest_address, wait_ready,
};
use gradient_wire::guest::{AgentToWorker, GuestMount, GuestNetwork, RunJob};
use gradient_wire::types::{ExecJob, MAX_PHASE_PAYLOAD};
use tokio::time::Instant;
use tracing::{debug, info, warn};

use super::agent_client::AgentClient;
use super::checkout::checkout_work;
use super::context::context_layers;
use super::host::ExecHost;
use super::inventory::device_requests;
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
}

#[derive(Debug)]
pub enum Followed {
    Exited(i32),
    Ended(BuildError),
    Stopped(Stop),
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
) -> Result<()> {
    let token = SecretString::new(std::mem::take(&mut job.context.token));
    let mut run = ExecRun {
        host,
        job,
        token,
        updater,
        abort,
    };
    run.ensure_program(store).await?;
    let devices = run.reserve_devices()?;
    let mut guest = run.take_guest().await?;
    let outcome = run.run_in_guest(&mut guest, &devices, credentials).await;
    guest.teardown(host.pool.units()).await;
    host.pool.refill();
    drop(devices);
    outcome
}

pub async fn finish<R: ExecReport>(
    host: &ExecHost,
    agent: &mut AgentClient,
    reporter: &mut R,
    watchdog: &mut Watchdog,
    abort: &mut AbortSignal,
) -> Result<()> {
    let stop = match follow(agent, reporter, watchdog, abort).await {
        Followed::Exited(code) => return exit_result(code).map_err(anyhow::Error::from),
        Followed::Ended(e) => return Err(e.into()),
        Followed::Stopped(stop) => stop,
    };
    if stop == Stop::Aborted && host.leaves_guest_running(abort) {
        info!("the worker stops and leaves the exec job running in its guest");
        std::future::pending::<()>().await;
    }
    if let Some(line) = stop.log_line()
        && let Err(e) = reporter.note(&line).await
    {
        warn!(error = %e, "could not log the timeout");
    }
    if let Err(e) = agent.kill().await {
        warn!(error = %e, "could not ask the guest agent to stop the program");
    }
    drain_until_exit(agent, reporter).await;
    Err(stop.into_error())
}

pub async fn follow<R: ExecReport>(
    agent: &mut AgentClient,
    reporter: &mut R,
    watchdog: &mut Watchdog,
    abort: &mut AbortSignal,
) -> Followed {
    let mut messages = agent.messages();
    loop {
        let message = match watchdog.next(&mut messages, abort).await {
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
        let guest = unless_aborted(&self.abort, self.host.pool.take()).await?;
        Ok(guest.map_err(BuildError::guest_boot)?)
    }

    async fn run_in_guest(
        &mut self,
        guest: &mut Guest,
        devices: &DeviceHold,
        credentials: &CredentialStore,
    ) -> Result<()> {
        let abort = self.abort.clone();
        let mut agent =
            unless_aborted(&abort, self.start_program(guest, devices, credentials)).await??;
        let mut watchdog = Watchdog::new(
            Instant::now(),
            self.job.timeout_secs.map(Duration::from_secs),
            self.job.max_silent_secs.map(Duration::from_secs),
        );
        finish(
            self.host,
            &mut agent,
            &mut *self.updater,
            &mut watchdog,
            &mut self.abort,
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
            secrets: Vec::new(),
            mounts: guest_mounts(mounts),
            groups: plugged.groups.clone(),
            network,
            relays: plugged.relays.clone(),
            usb_bus_ids: plugged.usb_bus_ids.clone(),
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
    use gradient_wire::guest::{AgentToWorker, GuestConnection};
    use gradient_wire::messages::BuildFailureKind;
    use tokio::net::UnixStream;

    use super::*;

    #[derive(Debug, PartialEq)]
    enum Reported {
        Started,
        Output(Vec<u8>),
        Dropped(u64),
        Phase(String),
        Note(String),
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
    }

    async fn followed(script: Vec<AgentToWorker>) -> (Followed, Vec<Reported>) {
        let (worker_end, agent_end) = UnixStream::pair().unwrap();
        let agent = async move {
            let mut conn = GuestConnection::agree(agent_end).await.unwrap();
            for message in script {
                conn.send(&message).await.unwrap();
            }
        };
        let (conn, ()) = tokio::join!(GuestConnection::agree(worker_end), agent);
        let mut agent = AgentClient::from(ReadyAgent {
            conn: conn.unwrap(),
            agent_version: "test".into(),
        });
        let mut recorder = Recorder::default();
        let mut watchdog = Watchdog::new(Instant::now(), None, None);
        let followed = follow(
            &mut agent,
            &mut recorder,
            &mut watchdog,
            &mut AbortSignal::never(),
        )
        .await;
        (followed, recorder.0)
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
