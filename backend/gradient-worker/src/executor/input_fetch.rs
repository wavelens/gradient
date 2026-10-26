/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::convert::Infallible;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use gradient_eval::pool::{DownloadSink, EvalPoolConfig, EvalWorker};
use gradient_util::sync::Mutex;
use gradient_wire::messages::{ClientMessage, PROGRESS_INTERVAL};
use gradient_wire::traits::JobReporter;
use gradient_wire::types::InputOutcome;
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
    pub ssh_key: Option<String>,
}

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
    let key_env = ssh_key_env(job.ssh_key.as_deref(), binpath_ssh).await?;
    let git_ssh_command = key_env.as_ref().map(|(_, command)| command.clone());
    let (transfers, totals) = Transfers::new();
    let download = downloader.download(job.request.clone(), git_ssh_command, transfers);
    let response = tokio::select! {
        downloaded = download => downloaded?,
        never = report_progress(totals, report) => match never {},
    };

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

async fn ssh_key_env(
    ssh_key: Option<&str>,
    binpath_ssh: &str,
) -> Result<Option<(tempfile::NamedTempFile, String)>> {
    use std::os::unix::fs::PermissionsExt;

    let Some(key) = ssh_key else {
        return Ok(None);
    };
    let kf = tempfile::NamedTempFile::with_suffix(".key").context("create SSH key temp file")?;
    tokio::fs::set_permissions(kf.path(), std::fs::Permissions::from_mode(0o600))
        .await
        .context("chmod SSH key file")?;
    tokio::fs::write(kf.path(), key.as_bytes())
        .await
        .context("write SSH key file")?;
    let ssh_command = format!(
        "{} -i {} -o IdentitiesOnly=yes -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null",
        binpath_ssh,
        kf.path().display()
    );
    Ok(Some((kf, ssh_command)))
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

    #[derive(Default)]
    struct FakeDownloader {
        answer: String,
        transfers: Vec<(u64, u64, u64)>,
        seen: Mutex<Vec<(Option<String>, Option<String>)>>,
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
            let key = git_ssh_command.as_deref().and_then(|command| {
                let path = command
                    .split_whitespace()
                    .skip_while(|w| *w != "-i")
                    .nth(1)?;
                std::fs::read_to_string(path).ok()
            });
            self.seen.lock().push((git_ssh_command, key));
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
    }

    fn job(ssh_key: Option<&str>) -> InputFetchJob {
        InputFetchJob {
            fetch_id: "f1".into(),
            project: "p1".into(),
            request: REQUEST.into(),
            ssh_key: ssh_key.map(str::to_owned),
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
    async fn the_ssh_key_reaches_the_fetch_as_git_ssh_command() {
        let downloader = FakeDownloader::answering(r#"{"storePath":"/nix/store/aaaa-source"}"#);
        let report = RecordedReport::default();

        fetch_input(job(Some("PRIVATE KEY")), &downloader, &report, "/bin/ssh").await;
        fetch_input(job(None), &downloader, &report, "/bin/ssh").await;

        let seen = downloader.seen.lock();
        let (command, key) = &seen[0];
        assert!(
            command
                .as_deref()
                .is_some_and(|c| c.starts_with("/bin/ssh -i ")),
            "{command:?}"
        );
        assert_eq!(key.as_deref(), Some("PRIVATE KEY"));
        assert_eq!(seen[1], (None, None), "no key, no ssh command");
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
