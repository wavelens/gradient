/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Concurrent transfers of one build count into one shared tally.
//! A retried or failed transfer gives back the bytes of its attempt.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use gradient_wire::messages::{ClientMessage, PROGRESS_INTERVAL};
use gradient_wire::traits::EvalProgressSink;
use gradient_wire::types::{BuildProgressPhase, EvalProgress};
use tokio::sync::watch;
use tokio::time::Instant;
use tracing::debug;

use gradient_worker_client::connection::ProtoWriter;
use gradient_worker_client::correlation::AssignmentHandle;
use gradient_worker_client::nar_recv::NarPayload;

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Transferred {
    pub(crate) bytes_done: u64,
    pub(crate) bytes_total: Option<u64>,
    pub(crate) paths_done: u32,
    pub(crate) paths_total: Option<u32>,
}

pub(crate) trait ProgressSink {
    async fn report(&mut self, transferred: Transferred);
}

impl ProgressSink for () {
    async fn report(&mut self, _: Transferred) {}
}

pub(crate) struct BuildProgressSink {
    pub(crate) writer: ProtoWriter,
    pub(crate) job_id: String,
    pub(crate) assignment_id: AssignmentHandle,
    pub(crate) build_id: String,
    pub(crate) phase: BuildProgressPhase,
}

impl ProgressSink for BuildProgressSink {
    async fn report(&mut self, transferred: Transferred) {
        let sent = self
            .writer
            .send(ClientMessage::BuildProgress {
                job_id: self.job_id.clone(),
                assignment_id: self.assignment_id.get(),
                build_id: self.build_id.clone(),
                phase: self.phase,
                bytes_done: transferred.bytes_done,
                bytes_total: transferred.bytes_total,
                paths_done: transferred.paths_done,
                paths_total: transferred.paths_total,
            })
            .await;
        if let Err(e) = sent {
            debug!(build_id = %self.build_id, error = %e, "build progress not sent");
        }
    }
}

pub(crate) struct EvalProgressSender {
    pub(crate) writer: ProtoWriter,
    pub(crate) job_id: String,
    pub(crate) assignment_id: AssignmentHandle,
}

#[async_trait::async_trait]
impl EvalProgressSink for EvalProgressSender {
    async fn report(&self, progress: EvalProgress) {
        let sent = self
            .writer
            .send(ClientMessage::EvalProgress {
                job_id: self.job_id.clone(),
                assignment_id: self.assignment_id.get(),
                progress,
            })
            .await;
        if let Err(e) = sent {
            debug!(job_id = %self.job_id, error = %e, "eval progress not sent");
        }
    }
}

#[derive(Clone, Default)]
pub(crate) struct Tally(Arc<Counts>);

#[derive(Default)]
struct Counts {
    bytes: AtomicU64,
    paths: AtomicU32,
}

impl Tally {
    pub(crate) fn count_transfer(&self, bytes: u64) {
        let mut transfer = self.transfer();
        transfer.at(bytes);
        transfer.done();
    }

    fn transfer(&self) -> TransferBytes {
        TransferBytes {
            tally: self.clone(),
            counted: 0,
        }
    }

    fn bytes(&self) -> u64 {
        self.0.bytes.load(Ordering::Relaxed)
    }

    fn paths(&self) -> u32 {
        self.0.paths.load(Ordering::Relaxed)
    }
}

struct TransferBytes {
    tally: Tally,
    counted: u64,
}

impl TransferBytes {
    fn at(&mut self, bytes: u64) {
        let total = &self.tally.0.bytes;
        if bytes >= self.counted {
            total.fetch_add(bytes - self.counted, Ordering::Relaxed);
        } else {
            total.fetch_sub(self.counted - bytes, Ordering::Relaxed);
        }
        self.counted = bytes;
    }

    fn done(mut self) {
        self.tally.0.paths.fetch_add(1, Ordering::Relaxed);
        self.counted = 0;
    }
}

impl Drop for TransferBytes {
    fn drop(&mut self) {
        self.tally
            .0
            .bytes
            .fetch_sub(self.counted, Ordering::Relaxed);
    }
}

pub(crate) struct Progress<S> {
    sink: S,
    tally: Tally,
    current: TransferBytes,
    bytes_total: Option<u64>,
    paths_total: u32,
    paths_known: bool,
    reported: Option<Transferred>,
    deadline: Instant,
}

impl Progress<()> {
    pub(crate) fn silent() -> Self {
        Self::new(())
    }

    pub(crate) fn counting(tally: Tally) -> Self {
        Self::with_tally((), tally)
    }
}

impl<S: ProgressSink> Progress<S> {
    pub(crate) fn new(sink: S) -> Self {
        Self::with_tally(sink, Tally::default())
    }

    fn with_tally(sink: S, tally: Tally) -> Self {
        Self {
            sink,
            current: tally.transfer(),
            tally,
            bytes_total: None,
            paths_total: 0,
            paths_known: false,
            reported: None,
            deadline: Instant::now() + PROGRESS_INTERVAL,
        }
    }

    pub(crate) fn tally(&self) -> Tally {
        self.tally.clone()
    }

    pub(crate) fn set_total(&mut self, bytes: Option<u64>, paths: u32) {
        self.bytes_total = bytes;
        self.paths_total = paths;
        self.paths_known = true;
    }

    pub(crate) fn expect(&mut self, bytes: Option<u64>, paths: u32) {
        self.bytes_total = match self.paths_total {
            0 => bytes,
            _ => self.bytes_total.zip(bytes).map(|(had, more)| had + more),
        };
        self.paths_total += paths;
    }

    pub(crate) fn at(&mut self, bytes: u64) {
        self.current.at(bytes);
    }

    pub(crate) fn transfer_done(&mut self) {
        let next = self.tally.transfer();
        std::mem::replace(&mut self.current, next).done();
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }

    pub(crate) async fn tick(&mut self) {
        self.deadline = Instant::now() + PROGRESS_INTERVAL;
        let now = self.transferred();
        if self.reported != Some(now) {
            self.reported = Some(now);
            self.sink.report(now).await;
        }
    }

    pub(crate) async fn during<T>(&mut self, work: impl Future<Output = T>) -> T {
        report_during(std::slice::from_mut(self), work).await
    }

    pub(crate) async fn finish(&mut self) {
        self.paths_known = true;
        let now = self.transferred();
        if self.paths_total == 0 && now.bytes_done == 0 {
            return;
        }
        self.reported = Some(now);
        self.sink.report(now).await;
    }

    fn transferred(&self) -> Transferred {
        Transferred {
            bytes_done: self.tally.bytes(),
            bytes_total: self.bytes_total,
            paths_done: self.tally.paths(),
            paths_total: self.paths_known.then_some(self.paths_total),
        }
    }
}

pub(crate) async fn report_during<S: ProgressSink, T>(
    progress: &mut [Progress<S>],
    work: impl Future<Output = T>,
) -> T {
    tokio::pin!(work);
    loop {
        let next = progress.iter().map(Progress::deadline).min();
        let due = async move {
            match next {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            out = &mut work => return out,
            () = due => {
                for p in progress.iter_mut().filter(|p| p.deadline() <= Instant::now()) {
                    p.tick().await;
                }
            }
        }
    }
}

pub(crate) async fn count_delivery(
    tally: Tally,
    mut received: watch::Receiver<u64>,
    delivery: impl Future<Output = anyhow::Result<NarPayload>>,
) -> anyhow::Result<NarPayload> {
    let mut counted = Progress::counting(tally);
    tokio::pin!(delivery);
    let payload = loop {
        tokio::select! {
            delivered = &mut delivery => break delivered?,
            Ok(()) = received.changed() => counted.at(*received.borrow_and_update()),
        }
    };
    counted.at(payload.byte_len().await);
    counted.transfer_done();
    Ok(payload)
}

const STAGE_CHUNK_BYTES: usize = 1 << 20;

pub(crate) async fn stage_body(
    mut response: reqwest::Response,
    sink: &mut gradient_worker_client::nar_recv::DownloadSink,
    progress: &mut Progress<impl ProgressSink>,
) -> anyhow::Result<()> {
    let mut buffered = Vec::with_capacity(STAGE_CHUNK_BYTES);
    let mut received = 0u64;
    loop {
        tokio::select! {
            chunk = response.chunk() => match chunk? {
                Some(chunk) => {
                    received += chunk.len() as u64;
                    buffered.extend_from_slice(&chunk);
                    if buffered.len() >= STAGE_CHUNK_BYTES {
                        sink.append(&buffered).await?;
                        buffered.clear();
                    }
                    progress.at(received);
                }
                None => return sink.append(&buffered).await,
            },
            _ = tokio::time::sleep_until(progress.deadline()) => progress.tick().await,
        }
    }
}

#[cfg(test)]
#[derive(Default)]
pub(crate) struct Recorded(pub(crate) Vec<Transferred>);

#[cfg(test)]
impl ProgressSink for &mut Recorded {
    async fn report(&mut self, transferred: Transferred) {
        self.0.push(transferred);
    }
}

#[cfg(test)]
pub(crate) fn transferred(
    bytes_done: u64,
    bytes_total: Option<u64>,
    paths_done: u32,
    paths_total: Option<u32>,
) -> Transferred {
    Transferred {
        bytes_done,
        bytes_total,
        paths_done,
        paths_total,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn a_deadline_reports_only_on_a_change_and_the_end_always_does() {
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);
        progress.set_total(Some(100), 1);

        progress.at(10);
        progress.tick().await;
        progress.tick().await;
        progress.at(40);
        progress.tick().await;
        progress.at(90);
        progress.finish().await;

        assert_eq!(
            sent.0,
            vec![
                transferred(10, Some(100), 0, Some(1)),
                transferred(40, Some(100), 0, Some(1)),
                transferred(90, Some(100), 0, Some(1)),
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_tick_moves_the_deadline_one_interval_on() {
        let mut progress = Progress::silent();
        tokio::time::advance(PROGRESS_INTERVAL * 3).await;

        progress.tick().await;

        assert_eq!(progress.deadline(), Instant::now() + PROGRESS_INTERVAL);
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

        assert_eq!(sent.0, vec![transferred(50, None, 1, Some(0))]);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_transfers_share_one_report_and_a_failed_one_gives_its_bytes_back() {
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);
        progress.expect(Some(50), 2);
        let mut landed = Progress::counting(progress.tally());
        let mut failed = Progress::counting(progress.tally());

        landed.at(10);
        failed.at(20);
        progress.tick().await;
        landed.transfer_done();
        drop(failed);
        progress.tick().await;

        assert_eq!(
            sent.0,
            vec![
                transferred(30, Some(50), 0, None),
                transferred(10, Some(50), 1, None)
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn growing_paths_stay_unknown_until_the_end() {
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);
        progress.expect(Some(10), 1);
        let mut transfer = Progress::counting(progress.tally());

        transfer.at(10);
        transfer.transfer_done();
        progress.tick().await;
        progress.finish().await;

        assert_eq!(
            sent.0,
            vec![
                transferred(10, Some(10), 1, None),
                transferred(10, Some(10), 1, Some(1))
            ]
        );
    }

    #[test]
    fn a_later_round_grows_the_totals_and_one_unknown_size_hides_the_bytes() {
        let mut progress = Progress::silent();

        progress.expect(Some(10), 1);
        progress.expect(Some(5), 2);
        assert_eq!(progress.transferred(), transferred(0, Some(15), 0, None));

        progress.expect(None, 1);
        progress.expect(Some(7), 1);
        assert_eq!(progress.transferred(), transferred(0, None, 0, None));
    }

    #[tokio::test(start_paused = true)]
    async fn work_is_reported_every_interval_while_it_runs() {
        let mut sent = Recorded::default();
        let mut progress = Progress::new(&mut sent);
        progress.set_total(Some(100), 1);
        let mut transfer = Progress::counting(progress.tally());

        progress
            .during(async move {
                transfer.at(50);
                tokio::time::sleep(PROGRESS_INTERVAL + PROGRESS_INTERVAL / 2).await;
                transfer.at(100);
                transfer.transfer_done();
            })
            .await;
        progress.finish().await;

        assert_eq!(
            sent.0,
            vec![
                transferred(50, Some(100), 0, Some(1)),
                transferred(100, Some(100), 1, Some(1))
            ]
        );
    }

    #[tokio::test]
    async fn nothing_expected_and_nothing_counted_sends_no_end() {
        let mut sent = Recorded::default();

        Progress::new(&mut sent).finish().await;

        assert!(sent.0.is_empty());
    }

    #[tokio::test]
    async fn a_nar_counts_its_staged_bytes_before_it_lands_and_a_failed_one_gives_them_back() {
        let tally = Tally::default();
        let (staged, received) = watch::channel(0);
        let (deliver, delivery) = tokio::sync::oneshot::channel();
        let landing = count_delivery(tally.clone(), received, async {
            delivery.await.expect("delivered")
        });
        tokio::pin!(landing);

        staged.send_replace(40);
        assert!(futures::poll!(&mut landing).is_pending());
        assert_eq!((tally.bytes(), tally.paths()), (40, 0));

        deliver.send(Ok(NarPayload::from(vec![0; 50]))).unwrap();
        landing.await.unwrap();
        assert_eq!((tally.bytes(), tally.paths()), (50, 1));

        let (staged, received) = watch::channel(0);
        let (abort, delivery) = tokio::sync::oneshot::channel();
        let failing = count_delivery(tally.clone(), received, async {
            delivery.await.expect("delivered")
        });
        tokio::pin!(failing);
        staged.send_replace(30);
        assert!(futures::poll!(&mut failing).is_pending());
        assert_eq!(tally.bytes(), 80);
        abort.send(Err(anyhow::anyhow!("aborted"))).unwrap();
        failing.await.unwrap_err();
        assert_eq!((tally.bytes(), tally.paths()), (50, 1));
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

        let mut body = gradient_worker_client::nar_recv::DownloadSink::memory();
        stage_body(response, &mut body, &mut progress)
            .await
            .unwrap();
        progress.finish().await;

        assert_eq!(body.len(), 300_000);
        assert_eq!(sent.0, vec![transferred(300_000, None, 0, Some(0))]);
    }
}
