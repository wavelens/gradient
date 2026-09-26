/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! A download's running byte count, reported at most once per
//! [`BUILD_PROGRESS_INTERVAL`]. The count spans every transfer of one build;
//! a retried transfer restarts from where the finished ones left off.

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
    last_report: Instant,
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
            last_report: Instant::now(),
        }
    }

    pub(crate) fn set_total(&mut self, total: Option<u64>) {
        self.total = total;
    }

    /// The running transfer has fetched `bytes` so far.
    pub(crate) async fn at(&mut self, bytes: u64) {
        self.current = bytes;
        let now = Instant::now();
        if now.duration_since(self.last_report) >= BUILD_PROGRESS_INTERVAL {
            self.last_report = now;
            self.sink.report(self.downloaded(), self.total).await;
        }
    }

    pub(crate) fn transfer_done(&mut self) {
        self.finished += self.current;
        self.current = 0;
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

/// Read a response body whole, reporting each chunk to `progress`.
pub(crate) async fn read_body(
    mut response: reqwest::Response,
    size_hint: Option<u64>,
    progress: &mut Progress<impl ProgressSink>,
) -> reqwest::Result<Vec<u8>> {
    let mut body = Vec::with_capacity(size_hint.unwrap_or(0).min(MAX_PREALLOCATION) as usize);
    while let Some(chunk) = response.chunk().await? {
        body.extend_from_slice(&chunk);
        progress.at(body.len() as u64).await;
    }
    Ok(body)
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
    use std::time::Duration;

    #[tokio::test(start_paused = true)]
    async fn reports_are_throttled_and_the_last_count_always_goes_out() {
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);
        progress.set_total(Some(100));

        progress.at(10).await;
        tokio::time::advance(BUILD_PROGRESS_INTERVAL).await;
        progress.at(40).await;
        tokio::time::advance(Duration::from_millis(10)).await;
        progress.at(90).await;
        progress.finish().await;

        assert_eq!(sent.0, vec![(40, Some(100)), (90, Some(100))]);
    }

    #[tokio::test(start_paused = true)]
    async fn a_retried_transfer_restarts_after_the_finished_ones() {
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);

        progress.at(30).await;
        progress.transfer_done();
        progress.at(50).await;
        progress.at(5).await;
        tokio::time::advance(BUILD_PROGRESS_INTERVAL).await;
        progress.at(20).await;
        progress.finish().await;

        assert_eq!(sent.0, vec![(50, None), (50, None)]);
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
        assert_eq!(sent.0.last(), Some(&(300_000, None)));
    }
}
