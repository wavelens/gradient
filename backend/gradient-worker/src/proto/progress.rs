/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A download's running byte count, reported at each [`BUILD_PROGRESS_INTERVAL`]
//! deadline by which bytes arrived, and once more when it finishes. The count
//! spans every transfer of one build; a retried transfer restarts from where
//! the finished ones left off.

use gradient_proto::messages::{BUILD_PROGRESS_INTERVAL, ClientMessage};
use tokio::time::Instant;
use tracing::debug;

use crate::connection::ProtoWriter;
use crate::proto::job::DispatchHandle;

pub(crate) trait ProgressSink {
    async fn report(&mut self, downloaded: u64, total: Option<u64>);
}

impl ProgressSink for () {
    async fn report(&mut self, _: u64, _: Option<u64>) {}
}

pub(crate) struct BuildProgressSink {
    pub(crate) writer: ProtoWriter,
    pub(crate) job_id: String,
    pub(crate) dispatch: DispatchHandle,
    pub(crate) build_id: String,
}

impl ProgressSink for BuildProgressSink {
    async fn report(&mut self, downloaded: u64, total: Option<u64>) {
        let sent = self
            .writer
            .send(ClientMessage::BuildProgress {
                job_id: self.job_id.clone(),
                dispatch: self.dispatch.get(),
                build_id: self.build_id.clone(),
                downloaded,
                total,
            })
            .await;
        if let Err(e) = sent {
            debug!(build_id = %self.build_id, error = %e, "build progress not sent");
        }
    }
}

pub(crate) struct Progress<S> {
    sink: S,
    total: Option<u64>,
    finished: u64,
    current: u64,
    reported: u64,
    deadline: Instant,
}

impl Progress<()> {
    pub(crate) fn silent() -> Self {
        Self::new(())
    }
}

impl<S: ProgressSink> Progress<S> {
    pub(crate) fn new(sink: S) -> Self {
        Self {
            sink,
            total: None,
            finished: 0,
            current: 0,
            reported: 0,
            deadline: Instant::now() + BUILD_PROGRESS_INTERVAL,
        }
    }

    pub(crate) fn set_total(&mut self, total: Option<u64>) {
        self.total = total;
    }

    /// The running transfer has fetched `bytes` so far.
    pub(crate) fn at(&mut self, bytes: u64) {
        self.current = bytes;
    }

    pub(crate) fn transfer_done(&mut self) {
        self.finished += self.current;
        self.current = 0;
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    /// Reports what arrived since the last report, if anything did.
    pub(crate) async fn tick(&mut self) {
        self.deadline = Instant::now() + BUILD_PROGRESS_INTERVAL;
        let downloaded = self.downloaded();
        if downloaded != self.reported {
            self.reported = downloaded;
            self.sink.report(downloaded, self.total).await;
        }
    }

    pub(crate) async fn finish(&mut self) {
        self.transfer_done();
        self.sink.report(self.finished, self.total).await;
    }

    fn downloaded(&self) -> u64 {
        self.finished + self.current
    }
}

/// A size a remote declared is a hint, never a reason to reserve unbounded memory.
const MAX_PREALLOCATION: u64 = 64 << 20;

/// Read a response body whole, ticking `progress` at each of its deadlines.
pub(crate) async fn read_body(
    mut response: reqwest::Response,
    size_hint: Option<u64>,
    progress: &mut Progress<impl ProgressSink>,
) -> reqwest::Result<Vec<u8>> {
    let mut body = Vec::with_capacity(size_hint.unwrap_or(0).min(MAX_PREALLOCATION) as usize);
    loop {
        tokio::select! {
            chunk = response.chunk() => match chunk? {
                Some(chunk) => {
                    body.extend_from_slice(&chunk);
                    progress.at(body.len() as u64);
                }
                None => return Ok(body),
            },
            _ = tokio::time::sleep_until(progress.deadline()) => progress.tick().await,
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct Recorded(pub(crate) Vec<(u64, Option<u64>)>);

#[cfg(test)]
impl ProgressSink for &mut Recorded {
    async fn report(&mut self, downloaded: u64, total: Option<u64>) {
        self.0.push((downloaded, total));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_deadline_reports_only_when_bytes_arrived_and_the_end_always_does() {
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);
        progress.set_total(Some(100));

        progress.at(10);
        progress.tick().await;
        progress.tick().await;
        progress.at(40);
        progress.tick().await;
        progress.at(90);
        progress.finish().await;

        assert_eq!(
            sent.0,
            vec![(10, Some(100)), (40, Some(100)), (90, Some(100))]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_tick_moves_the_deadline_one_interval_on() {
        let mut progress = Progress::silent();
        tokio::time::advance(BUILD_PROGRESS_INTERVAL * 3).await;

        progress.tick().await;

        assert_eq!(
            progress.deadline(),
            Instant::now() + BUILD_PROGRESS_INTERVAL
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_retried_transfer_restarts_after_the_finished_ones() {
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);

        progress.at(30);
        progress.transfer_done();
        progress.at(50);
        progress.at(5);
        progress.at(20);
        progress.finish().await;

        assert_eq!(sent.0, vec![(50, None)]);
    }

    #[tokio::test]
    async fn every_body_byte_is_counted() {
        use wiremock::matchers::method;
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(vec![7u8; 300_000]))
            .mount(&server)
            .await;
        let http = gradient_util::http::build_download_client().expect("download client");
        let response = http.get(server.uri()).send().await.unwrap();
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);

        let body = read_body(response, Some(u64::MAX), &mut progress)
            .await
            .unwrap();
        progress.finish().await;

        assert_eq!(body.len(), 300_000);
        assert_eq!(sent.0, vec![(300_000, None)]);
    }
}
