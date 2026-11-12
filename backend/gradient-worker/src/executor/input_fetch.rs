/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use gradient_eval::pool::{DownloadSink, EvalPoolConfig, EvalWorker};
use gradient_types::SecretString;
use gradient_util::sync::Mutex;
use gradient_wire::messages::{
    ClientMessage, FIRST_WORKER_PROTOCOL_VERSION_WITH_SSH_HOST_KEYS, GitServerSshHostKey,
    GitSshHostKeyObservation, PROGRESS_INTERVAL, WorkerReportAssignment,
};
use gradient_wire::traits::JobReporter;
use gradient_wire::types::InputOutcome;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tokio::time::MissedTickBehavior;
use tracing::{info, warn};

use crate::proto::job::JobUpdater;
use gradient_worker_client::connection::ProtoWriter;

#[derive(Debug, Clone)]
pub struct InputFetchJob {
    pub fetch_id: String,
    pub project: String,
    pub request: String,
    pub ssh_key: Option<SecretString>,
    pub ssh_host_keys: Vec<GitServerSshHostKey>,
}

const NIX_FETCH_SSH_PRIVATE_KEY_FILE_NAME: &str = "key";
const FETCH_SSH_TEMPORARY_KNOWN_HOSTS_FILE_NAME: &str = "known_hosts";
const NIX_FETCH_SSH_LOG_FILE_NAME: &str = "ssh.log";

#[async_trait]
pub(crate) trait InputDownloader: Send + Sync {
    async fn download(
        &self,
        request: String,
        git_ssh_command: Option<String>,
        sink: Arc<dyn DownloadSink>,
    ) -> Result<String>;
}

#[async_trait]
pub(crate) trait FetchReport: Send + Sync {
    async fn progress(&self, bytes_done: u64, bytes_total: Option<u64>);

    async fn push(&self, store_path: String) -> Result<()>;

    async fn report_git_ssh_host_key_observations(
        &self,
        observations: Vec<GitSshHostKeyObservation>,
    ) -> Result<()>;
}

pub(crate) struct SubprocessDownloader(pub EvalPoolConfig);

#[async_trait]
impl InputDownloader for SubprocessDownloader {
    async fn download(
        &self,
        request: String,
        git_ssh_command: Option<String>,
        sink: Arc<dyn DownloadSink>,
    ) -> Result<String> {
        let mut worker = EvalWorker::spawn_for_fetch(&self.0).await?;
        let fetched = worker.fetch(request, git_ssh_command, sink).await;
        worker.shutdown().await;
        fetched
    }
}

pub(crate) struct UpdaterFetchReport {
    pub(crate) updater: JobUpdater,
    pub(crate) session: ProtoWriter,
}

#[async_trait]
impl FetchReport for UpdaterFetchReport {
    async fn progress(&self, bytes_done: u64, bytes_total: Option<u64>) {
        let sent = self
            .session
            .send(ClientMessage::InputFetchProgress {
                fetch_id: self.updater.job_id.clone(),
                bytes_done,
                bytes_total,
            })
            .await;
        if let Err(e) = sent {
            warn!(fetch_id = %self.updater.job_id, error = %e, "input fetch progress not sent");
        }
    }

    async fn push(&self, store_path: String) -> Result<()> {
        JobReporter::push_paths(&self.updater, &[(store_path, None)]).await
    }

    async fn report_git_ssh_host_key_observations(
        &self,
        observations: Vec<GitSshHostKeyObservation>,
    ) -> Result<()> {
        if observations.is_empty()
            || self.session.version() < FIRST_WORKER_PROTOCOL_VERSION_WITH_SSH_HOST_KEYS
        {
            return Ok(());
        }
        self.session
            .send(ClientMessage::SshHostKeyObservations {
                assignment: WorkerReportAssignment::Fetch {
                    fetch_id: self.updater.job_id.clone(),
                },
                observations,
            })
            .await
    }
}

pub(crate) async fn fetch_input(
    job: InputFetchJob,
    downloader: &dyn InputDownloader,
    report: &dyn FetchReport,
    binpath_ssh: &str,
) -> InputOutcome {
    info!(fetch_id = %job.fetch_id, project = %job.project, "fetching an input");
    match fetch_and_push(&job, downloader, report, binpath_ssh).await {
        Ok(response) => InputOutcome::Fetched { response },
        Err(e) => InputOutcome::Failed {
            message: format!("{e:#}"),
        },
    }
}

async fn fetch_and_push(
    job: &InputFetchJob,
    downloader: &dyn InputDownloader,
    report: &dyn FetchReport,
    binpath_ssh: &str,
) -> Result<String> {
    let ssh = write_known_hosts_and_build_nix_fetch_ssh_command(
        job.ssh_key.as_ref().map(SecretString::expose),
        &job.ssh_host_keys,
        binpath_ssh,
    )
    .await?;
    let git_ssh_command = ssh.as_ref().map(|(_, command)| command.clone());
    let (transfers, totals) = Transfers::new();
    let download = downloader.download(job.request.clone(), git_ssh_command, transfers);
    let downloaded = tokio::select! {
        downloaded = download => downloaded,
        never = report_progress(totals, report) => match never {},
    };
    if let Some((dir, _)) = &ssh {
        let observations = read_host_key_observations_from_temp_files_after_nix_input_fetch(
            dir.path(),
            &job.ssh_host_keys,
        )
        .await;
        if let Err(error) = report
            .report_git_ssh_host_key_observations(observations)
            .await
        {
            warn!(fetch_id = %job.fetch_id, %error, "Input fetch did not send host key observations");
        }
    }
    let response = downloaded?;

    let store_path = checked_store_path(&job.request, &response)?;
    report
        .push(store_path.clone())
        .await
        .with_context(|| format!("push {store_path} to the cache"))?;
    Ok(response)
}

fn checked_store_path(request: &str, response: &str) -> Result<String> {
    let request: serde_json::Value = serde_json::from_str(request).context("parse the request")?;
    let response: serde_json::Value =
        serde_json::from_str(response).context("parse the fetch answer")?;
    let fetched = response["storePath"]
        .as_str()
        .context("a fetch answer without a store path")?;
    if let Some(expected) = request["storePath"].as_str()
        && expected != fetched
    {
        anyhow::bail!("the input was fetched as {fetched}, expected {expected}");
    }
    Ok(fetched.to_owned())
}

async fn write_known_hosts_and_build_nix_fetch_ssh_command(
    ssh_key: Option<&str>,
    host_keys: &[GitServerSshHostKey],
    binpath_ssh: &str,
) -> Result<Option<(tempfile::TempDir, String)>> {
    let Some(key) = ssh_key else {
        return Ok(None);
    };
    let dir =
        tempfile::tempdir().context("Create a temporary directory for SSH keys and host keys")?;
    let key_path = dir.path().join(NIX_FETCH_SSH_PRIVATE_KEY_FILE_NAME);
    let known_hosts = dir.path().join(FETCH_SSH_TEMPORARY_KNOWN_HOSTS_FILE_NAME);
    let log = dir.path().join(NIX_FETCH_SSH_LOG_FILE_NAME);

    let mut key_file = tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&key_path)
        .await
        .context("Write SSH key file")?;
    key_file
        .write_all(key.as_bytes())
        .await
        .context("Write SSH key file")?;
    key_file.flush().await.context("Write SSH key file")?;
    let lines: String = host_keys
        .iter()
        .map(|key| format_known_hosts_line_for_saved_git_server_host_key(key) + "\n")
        .collect();
    tokio::fs::write(&known_hosts, lines)
        .await
        .context("Write the known_hosts file")?;

    let ssh_command = format!(
        "{binpath_ssh} -i {} -o IdentitiesOnly=yes -o BatchMode=yes -o UserKnownHostsFile={} -o GlobalKnownHostsFile=/dev/null -o StrictHostKeyChecking=accept-new -o HashKnownHosts=no -E {}",
        key_path.display(),
        known_hosts.display(),
        log.display()
    );
    Ok(Some((dir, ssh_command)))
}

fn format_known_hosts_line_for_saved_git_server_host_key(key: &GitServerSshHostKey) -> String {
    match key.port {
        22 => format!("{} {}", key.host, key.public_key),
        port => format!("[{}]:{port} {}", key.host, key.public_key),
    }
}

fn parse_ssh_host_spec_to_lowercase_host_and_port(spec: &str) -> Option<(String, u16)> {
    let first = spec.split(',').next()?;
    let (host, port) = match first.strip_prefix('[') {
        Some(bracketed) => {
            let (host, port) = bracketed.split_once("]:")?;
            (host, port.parse().ok()?)
        }
        None => (first, 22),
    };
    (!host.is_empty()).then(|| (host.to_lowercase(), port))
}

async fn read_host_key_observations_from_temp_files_after_nix_input_fetch(
    dir: &Path,
    written: &[GitServerSshHostKey],
) -> Vec<GitSshHostKeyObservation> {
    let read = |name| async move {
        tokio::fs::read_to_string(dir.join(name))
            .await
            .unwrap_or_default()
    };
    let mut observations = find_host_keys_added_by_ssh(
        &read(FETCH_SSH_TEMPORARY_KNOWN_HOSTS_FILE_NAME).await,
        written,
    );
    observations.extend(find_refused_changed_host_keys_in_ssh_log(
        &read(NIX_FETCH_SSH_LOG_FILE_NAME).await,
    ));
    observations
}

fn find_host_keys_added_by_ssh(
    known_hosts: &str,
    written: &[GitServerSshHostKey],
) -> Vec<GitSshHostKeyObservation> {
    let written: HashSet<String> = written
        .iter()
        .map(format_known_hosts_line_for_saved_git_server_host_key)
        .collect();
    known_hosts
        .lines()
        .filter(|line| !written.contains(*line))
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let (host, port) = parse_ssh_host_spec_to_lowercase_host_and_port(fields.next()?)?;
            let public_key = format!("{} {}", fields.next()?, fields.next()?);
            Some(GitSshHostKeyObservation::UnknownHostKeyAcceptedForSaving {
                key: GitServerSshHostKey {
                    host,
                    port,
                    public_key,
                },
            })
        })
        .collect()
}

fn find_refused_changed_host_keys_in_ssh_log(log: &str) -> Vec<GitSshHostKeyObservation> {
    log.lines()
        .filter_map(|line| {
            let (_, rest) = line.split_once("Host key for ")?;
            let (spec, _) = rest.split_once(" has changed")?;
            let (host, port) = parse_ssh_host_spec_to_lowercase_host_and_port(spec)?;
            Some(GitSshHostKeyObservation::HostKeyMismatchConnectionRefused { host, port })
        })
        .collect()
}

type Totals = (u64, Option<u64>);

struct Transfers {
    sizes: Mutex<HashMap<u64, (u64, u64)>>,
    totals: watch::Sender<Totals>,
}

impl Transfers {
    fn new() -> (Arc<Self>, watch::Receiver<Totals>) {
        let (totals, rx) = watch::channel((0, None));
        let transfers = Arc::new(Self {
            sizes: Mutex::default(),
            totals,
        });
        (transfers, rx)
    }
}

impl DownloadSink for Transfers {
    fn transfer(&self, id: u64, done: u64, expected: u64) {
        let mut sizes = self.sizes.lock();
        sizes.insert(id, (done, expected));
        let done = sizes.values().map(|(done, _)| done).sum();
        let total = sizes
            .values()
            .all(|(_, expected)| *expected > 0)
            .then(|| sizes.values().map(|(_, expected)| expected).sum());
        self.totals.send_replace((done, total));
    }
}

async fn report_progress(
    mut totals: watch::Receiver<Totals>,
    report: &dyn FetchReport,
) -> Infallible {
    let mut ticks = tokio::time::interval(PROGRESS_INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        if totals.has_changed().unwrap_or(false) {
            let (done, total) = *totals.borrow_and_update();
            report.progress(done, total).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: &str = r#"{"builtin":"fetchTree","args":{"type":"git","url":"git+ssh://git@example.com/r"},"storePath":"/nix/store/aaaa-source"}"#;
    const FETCHED: &str = r#"{"storePath":"/nix/store/aaaa-source"}"#;

    #[derive(Default)]
    struct FakeDownloader {
        answer: String,
        transfers: Vec<(u64, u64, u64)>,
        known_hosts_line_to_append: Option<String>,
        ssh_log_text_before_host_key_failure: Option<String>,
        seen: Mutex<Vec<FakeDownloaderSawSshCommandPrivateKeyAndKnownHostsContentsForOneDownload>>,
    }

    type FakeDownloaderSawSshCommandPrivateKeyAndKnownHostsContentsForOneDownload =
        (Option<String>, Option<String>, Option<String>);

    fn value_after_option_in_ssh_command<'a>(command: &'a str, option: &str) -> Option<&'a str> {
        let mut words = command.split_whitespace();
        match option.ends_with('=') {
            true => words.find_map(|w| w.strip_prefix(option)),
            false => words.skip_while(|w| *w != option).nth(1),
        }
    }

    impl FakeDownloader {
        fn answering(answer: &str) -> Self {
            Self {
                answer: answer.to_owned(),
                ..Self::default()
            }
        }
    }

    #[async_trait]
    impl InputDownloader for FakeDownloader {
        async fn download(
            &self,
            _request: String,
            git_ssh_command: Option<String>,
            sink: Arc<dyn DownloadSink>,
        ) -> Result<String> {
            let path = |option| {
                git_ssh_command
                    .as_deref()
                    .and_then(|command| value_after_option_in_ssh_command(command, option))
            };
            let read = |option| path(option).and_then(|p| std::fs::read_to_string(p).ok());
            let (key, known_hosts) = (read("-i"), read("UserKnownHostsFile="));
            if let (Some(line), Some(known_hosts)) = (
                &self.known_hosts_line_to_append,
                path("UserKnownHostsFile="),
            ) {
                let mut file = std::fs::OpenOptions::new().append(true).open(known_hosts)?;
                std::io::Write::write_all(&mut file, line.as_bytes())?;
            }
            if let (Some(text), Some(log)) =
                (&self.ssh_log_text_before_host_key_failure, path("-E"))
            {
                std::fs::write(log, text)?;
                anyhow::bail!("Host key verification failed.");
            }
            self.seen
                .lock()
                .push((git_ssh_command.clone(), key, known_hosts));
            for &(id, done, expected) in &self.transfers {
                sink.transfer(id, done, expected);
            }
            tokio::time::sleep(PROGRESS_INTERVAL * 3).await;
            Ok(self.answer.clone())
        }
    }

    #[derive(Default)]
    struct RecordedReport {
        progress: Mutex<Vec<Totals>>,
        pushed: Mutex<Vec<String>>,
        received_host_key_observations: Mutex<Vec<GitSshHostKeyObservation>>,
    }

    #[async_trait]
    impl FetchReport for RecordedReport {
        async fn progress(&self, bytes_done: u64, bytes_total: Option<u64>) {
            self.progress.lock().push((bytes_done, bytes_total));
        }

        async fn push(&self, store_path: String) -> Result<()> {
            self.pushed.lock().push(store_path);
            Ok(())
        }

        async fn report_git_ssh_host_key_observations(
            &self,
            observations: Vec<GitSshHostKeyObservation>,
        ) -> Result<()> {
            self.received_host_key_observations
                .lock()
                .extend(observations);
            Ok(())
        }
    }

    fn job(ssh_key: Option<&str>) -> InputFetchJob {
        InputFetchJob {
            fetch_id: "f1".into(),
            project: "p1".into(),
            request: REQUEST.into(),
            ssh_key: ssh_key.map(|key| SecretString::new(key.to_owned())),
            ssh_host_keys: vec![
                GitServerSshHostKey {
                    host: "git.example.org".into(),
                    port: 2222,
                    public_key: "ssh-ed25519 AAAAsaved2222".into(),
                },
                GitServerSshHostKey {
                    host: "git.elektrat.de".into(),
                    port: 22,
                    public_key: "ssh-ed25519 AAAAsaved22".into(),
                },
            ],
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_fetched_input_is_pushed_and_answered() {
        let downloader = FakeDownloader::answering(r#"{"storePath":"/nix/store/aaaa-source"}"#);
        let report = RecordedReport::default();

        let outcome = fetch_input(job(None), &downloader, &report, "ssh").await;

        assert_eq!(
            outcome,
            InputOutcome::Fetched {
                response: r#"{"storePath":"/nix/store/aaaa-source"}"#.into()
            }
        );
        assert_eq!(
            *report.pushed.lock(),
            vec!["/nix/store/aaaa-source".to_owned()]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_response_with_another_store_path_fails() {
        let downloader = FakeDownloader::answering(r#"{"storePath":"/nix/store/bbbb-source"}"#);
        let report = RecordedReport::default();

        let outcome = fetch_input(job(None), &downloader, &report, "ssh").await;

        assert_eq!(
            outcome,
            InputOutcome::Failed {
                message: "the input was fetched as /nix/store/bbbb-source, expected /nix/store/aaaa-source".into()
            }
        );
        assert!(report.pushed.lock().is_empty(), "nothing pushed");
    }

    #[tokio::test(start_paused = true)]
    async fn the_nix_fetch_ssh_command_uses_only_the_saved_host_keys() {
        let downloader = FakeDownloader::answering(FETCHED);
        let report = RecordedReport::default();

        fetch_input(job(Some("PRIVATE KEY")), &downloader, &report, "/bin/ssh").await;
        fetch_input(job(None), &downloader, &report, "/bin/ssh").await;

        let seen = downloader.seen.lock();
        let (command, key, known_hosts) = &seen[0];
        let command = command.as_deref().unwrap_or_default();
        assert!(command.starts_with("/bin/ssh -i "), "{command}");
        for option in [
            "-o UserKnownHostsFile=",
            "-o GlobalKnownHostsFile=/dev/null",
            "-o StrictHostKeyChecking=accept-new",
        ] {
            assert!(command.contains(option), "{option} in {command}");
        }
        assert_eq!(key.as_deref(), Some("PRIVATE KEY"));
        assert_eq!(
            known_hosts.as_deref(),
            Some(
                "[git.example.org]:2222 ssh-ed25519 AAAAsaved2222\ngit.elektrat.de ssh-ed25519 AAAAsaved22\n"
            )
        );
        assert_eq!(seen[1], (None, None, None), "no key, no ssh command");
        assert!(report.received_host_key_observations.lock().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn lines_added_by_ssh_are_reported_as_learned_keys() {
        let downloader = FakeDownloader {
            known_hosts_line_to_append: Some(
                "[New.Example.org]:2222 ssh-ed25519 AAAAlearned\n".into(),
            ),
            ..FakeDownloader::answering(FETCHED)
        };
        let report = RecordedReport::default();

        fetch_input(job(Some("PRIVATE KEY")), &downloader, &report, "ssh").await;

        assert_eq!(
            *report.received_host_key_observations.lock(),
            vec![GitSshHostKeyObservation::UnknownHostKeyAcceptedForSaving {
                key: GitServerSshHostKey {
                    host: "new.example.org".into(),
                    port: 2222,
                    public_key: "ssh-ed25519 AAAAlearned".into(),
                }
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_changed_host_key_in_the_ssh_log_is_reported() {
        let downloader = FakeDownloader {
            ssh_log_text_before_host_key_failure: Some(
                "Host key for [git.example.org]:2222 has changed and you have requested strict checking.\nHost key verification failed.\n".into(),
            ),
            ..FakeDownloader::answering(FETCHED)
        };
        let report = RecordedReport::default();

        let outcome = fetch_input(job(Some("PRIVATE KEY")), &downloader, &report, "ssh").await;

        assert!(
            matches!(outcome, InputOutcome::Failed { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            *report.received_host_key_observations.lock(),
            vec![GitSshHostKeyObservation::HostKeyMismatchConnectionRefused {
                host: "git.example.org".into(),
                port: 2222,
            }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn download_progress_is_sent_as_input_fetch_progress() {
        let downloader = FakeDownloader {
            transfers: vec![(1, 100, 400), (2, 50, 100)],
            ..FakeDownloader::answering(r#"{"storePath":"/nix/store/aaaa-source"}"#)
        };
        let report = RecordedReport::default();

        fetch_input(job(None), &downloader, &report, "ssh").await;

        assert_eq!(*report.progress.lock(), vec![(150, Some(500))]);
    }
}
