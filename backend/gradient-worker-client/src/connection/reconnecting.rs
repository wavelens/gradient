/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::VecDeque;
use std::sync::Arc;

use anyhow::Result;
use gradient_util::sync::Mutex;
use gradient_wire::messages::ClientMessage;
use tokio::sync::watch;

use super::ProtoWriter;

#[derive(Clone, Default)]
pub struct ReconnectingWriter {
    inner: Arc<Inner>,
}

#[derive(Default)]
struct Inner {
    state: Mutex<State>,
    send_order: tokio::sync::Mutex<()>,
    session: watch::Sender<Session>,
}

enum State {
    Attached {
        writer: ProtoWriter,
        session: u64,
        sent_reports: Vec<ClientMessage>,
    },
    Detached {
        queue: VecDeque<ClientMessage>,
    },
}

impl Default for State {
    fn default() -> Self {
        State::Detached {
            queue: VecDeque::new(),
        }
    }
}

#[derive(Clone, Copy, Default)]
struct Session {
    number: u64,
    version: Option<u16>,
}

#[derive(Debug)]
pub struct SessionLost;

impl std::fmt::Display for SessionLost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the session to the server is lost")
    }
}
impl std::error::Error for SessionLost {}

impl ReconnectingWriter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn attached_to(writer: ProtoWriter) -> Self {
        let session = Session {
            number: 1,
            version: Some(writer.version()),
        };
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State::Attached {
                    writer,
                    session: session.number,
                    sent_reports: Vec::new(),
                }),
                send_order: tokio::sync::Mutex::new(()),
                session: watch::Sender::new(session),
            }),
        }
    }

    pub async fn attach(&self, writer: ProtoWriter) -> Result<()> {
        let _order = self.inner.send_order.lock().await;
        let mut queue = match &mut *self.inner.state.lock() {
            State::Detached { queue } => std::mem::take(queue),
            State::Attached { .. } => VecDeque::new(),
        };
        let mut sent_reports = Vec::new();
        while let Some(msg) = queue.pop_front() {
            if let Err(e) = writer.send(msg.clone()).await {
                let mut requeued = VecDeque::from(sent_reports);
                requeued.push_back(msg);
                requeued.extend(queue);
                *self.inner.state.lock() = State::Detached { queue: requeued };
                return Err(e);
            }
            if final_report_job(&msg).is_some() {
                sent_reports.push(msg);
            }
        }
        let number = self.inner.session.borrow().number + 1;
        let version = writer.version();
        *self.inner.state.lock() = State::Attached {
            writer,
            session: number,
            sent_reports,
        };
        self.inner.session.send_replace(Session {
            number,
            version: Some(version),
        });
        Ok(())
    }

    pub fn detach(&self) {
        let mut state = self.inner.state.lock();
        if let State::Attached { sent_reports, .. } = &mut *state {
            *state = State::Detached {
                queue: std::mem::take(sent_reports).into(),
            };
            self.mark_detached();
        }
    }

    pub fn forget_reports(&self, job_id: &str) {
        let other_job = |msg: &ClientMessage| final_report_job(msg) != Some(job_id);
        match &mut *self.inner.state.lock() {
            State::Attached { sent_reports, .. } => sent_reports.retain(other_job),
            State::Detached { queue } => queue.retain(other_job),
        }
    }

    pub async fn send(&self, msg: ClientMessage) -> Result<()> {
        let _order = self.inner.send_order.lock().await;
        let Some((writer, session)) = self.current() else {
            self.enqueue(msg);
            return Ok(());
        };
        if writer.send(msg.clone()).await.is_err() {
            self.lose(session);
            self.enqueue(msg);
        } else if final_report_job(&msg).is_some() {
            self.keep_sent_report(msg);
        }
        Ok(())
    }

    pub async fn send_now(&self, msg: ClientMessage) -> Result<()> {
        let _order = self.inner.send_order.lock().await;
        let (writer, session) = self.current().ok_or(SessionLost)?;
        if writer.send(msg).await.is_err() {
            self.lose(session);
            return Err(SessionLost.into());
        }
        Ok(())
    }

    pub fn version(&self) -> Option<u16> {
        self.inner.session.borrow().version
    }

    pub fn queued_final_reports(&self) -> Vec<String> {
        match &*self.inner.state.lock() {
            State::Detached { queue } => queue
                .iter()
                .filter_map(|msg| match msg {
                    ClientMessage::JobCompleted { assignment_id, .. }
                    | ClientMessage::JobFailed { assignment_id, .. } => Some(assignment_id.clone()),
                    _ => None,
                })
                .collect(),
            State::Attached { .. } => Vec::new(),
        }
    }

    pub fn current_session(&self) -> Option<u64> {
        let current = *self.inner.session.borrow();
        current.version.map(|_| current.number)
    }

    pub fn is_current(&self, session: u64) -> bool {
        let current = *self.inner.session.borrow();
        current.number == session && current.version.is_some()
    }

    pub async fn attached(&self) -> u64 {
        let mut rx = self.inner.session.subscribe();
        let session = *rx
            .wait_for(|s| s.version.is_some())
            .await
            .expect("the sender lives as long as the writer");
        session.number
    }

    pub async fn lost(&self, session: u64) {
        let mut rx = self.inner.session.subscribe();
        let _ = rx
            .wait_for(|s| s.number != session || s.version.is_none())
            .await;
    }

    fn current(&self) -> Option<(ProtoWriter, u64)> {
        match &*self.inner.state.lock() {
            State::Attached {
                writer, session, ..
            } => Some((writer.clone(), *session)),
            State::Detached { .. } => None,
        }
    }

    fn lose(&self, lost: u64) {
        let mut state = self.inner.state.lock();
        if let State::Attached {
            session,
            sent_reports,
            ..
        } = &mut *state
            && *session == lost
        {
            *state = State::Detached {
                queue: std::mem::take(sent_reports).into(),
            };
            self.mark_detached();
        }
    }

    fn keep_sent_report(&self, msg: ClientMessage) {
        match &mut *self.inner.state.lock() {
            State::Attached { sent_reports, .. } => sent_reports.push(msg),
            State::Detached { queue } => queue.push_back(msg),
        }
    }

    fn mark_detached(&self) {
        self.inner.session.send_modify(|s| s.version = None);
    }

    fn enqueue(&self, msg: ClientMessage) {
        let mut state = self.inner.state.lock();
        let State::Detached { queue } = &mut *state else {
            unreachable!("only a detached writer queues");
        };
        match queue.iter_mut().find(|queued| supersedes(queued, &msg)) {
            Some(queued) => *queued = msg,
            None => queue.push_back(msg),
        }
    }
}

fn final_report_job(msg: &ClientMessage) -> Option<&str> {
    match msg {
        ClientMessage::JobCompleted { job_id, .. } | ClientMessage::JobFailed { job_id, .. } => {
            Some(job_id)
        }
        _ => None,
    }
}

fn supersedes(queued: &ClientMessage, msg: &ClientMessage) -> bool {
    matches!(
        (queued, msg),
        (
            ClientMessage::BuildProgress { job_id: a, .. },
            ClientMessage::BuildProgress { job_id: b, .. },
        ) | (
            ClientMessage::EvalProgress { job_id: a, .. },
            ClientMessage::EvalProgress { job_id: b, .. },
        ) if a == b
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::mock_session as session;
    use gradient_wire::messages::{EvalProgress, JobUpdateKind};
    use std::time::Duration;

    fn update(job_id: &str) -> ClientMessage {
        ClientMessage::JobUpdate {
            job_id: job_id.into(),
            assignment_id: "a".into(),
            update: JobUpdateKind::Fetching,
        }
    }

    fn progress(job_id: &str, thunks: u64) -> ClientMessage {
        ClientMessage::EvalProgress {
            job_id: job_id.into(),
            assignment_id: "a".into(),
            progress: EvalProgress::Evaluating { thunks },
        }
    }

    fn completed() -> ClientMessage {
        ClientMessage::JobCompleted {
            job_id: "j".into(),
            assignment_id: "a".into(),
            spans: vec![],
            elapsed_ms: 1,
        }
    }

    #[tokio::test]
    async fn a_message_sent_while_detached_arrives_after_attach_in_order() {
        let w = ReconnectingWriter::new();
        w.send(update("one")).await.unwrap();
        w.send(update("two")).await.unwrap();

        let mut peer = session().await;
        w.attach(peer.writer.clone()).await.unwrap();

        assert_eq!(peer.server.recv().await.unwrap(), update("one"));
        assert_eq!(peer.server.recv().await.unwrap(), update("two"));
    }

    #[tokio::test]
    async fn a_message_whose_send_fails_is_queued_not_lost() {
        let w = ReconnectingWriter::new();
        let mut closed = session().await;
        w.attach(closed.writer.clone()).await.unwrap();
        closed.flush.close();
        assert!(closed.server.recv().await.is_err());

        w.send(completed()).await.unwrap();

        let mut next = session().await;
        w.attach(next.writer.clone()).await.unwrap();
        let received = tokio::time::timeout(Duration::from_secs(5), next.server.recv())
            .await
            .expect("the failed report reaches the next session");
        assert_eq!(received.unwrap(), completed());
    }

    #[tokio::test]
    async fn queued_progress_of_a_job_keeps_only_the_newest() {
        let w = ReconnectingWriter::new();
        w.send(progress("j", 1)).await.unwrap();
        w.send(progress("j", 2)).await.unwrap();
        w.send(progress("k", 1)).await.unwrap();

        let mut peer = session().await;
        w.attach(peer.writer.clone()).await.unwrap();

        assert_eq!(peer.server.recv().await.unwrap(), progress("j", 2));
        assert_eq!(peer.server.recv().await.unwrap(), progress("k", 1));
        assert!(
            tokio::time::timeout(Duration::from_millis(200), peer.server.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn send_now_refuses_while_detached() {
        let w = ReconnectingWriter::new();
        let refused = w
            .send_now(ClientMessage::UploadChunk {
                request_id: 1,
                data: bytes::Bytes::from_static(b"x"),
                offset: 0,
                is_final: true,
            })
            .await
            .unwrap_err();
        assert!(refused.downcast_ref::<SessionLost>().is_some());

        let mut peer = session().await;
        w.attach(peer.writer.clone()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), peer.server.recv())
                .await
                .is_err(),
            "a refused chunk is not queued"
        );
    }

    #[tokio::test]
    async fn a_final_report_sent_into_an_ended_session_is_resent_after_attach() {
        let w = ReconnectingWriter::new();
        let first = session().await;
        w.attach(first.writer.clone()).await.unwrap();
        w.send(completed()).await.unwrap();

        w.detach();
        assert_eq!(w.queued_final_reports(), vec!["a".to_owned()]);

        let mut next = session().await;
        w.attach(next.writer.clone()).await.unwrap();
        let received = tokio::time::timeout(Duration::from_secs(5), next.server.recv())
            .await
            .expect("the dead session may have swallowed the report");
        assert_eq!(received.unwrap(), completed());
    }

    #[tokio::test]
    async fn a_forgotten_report_is_neither_listed_nor_resent() {
        let w = ReconnectingWriter::new();
        let first = session().await;
        w.attach(first.writer.clone()).await.unwrap();
        w.send(completed()).await.unwrap();

        w.forget_reports("j");
        w.detach();
        assert!(w.queued_final_reports().is_empty());

        let mut next = session().await;
        w.attach(next.writer.clone()).await.unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(200), next.server.recv())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn lost_resolves_when_the_session_detaches() {
        let w = ReconnectingWriter::new();
        let first = session().await;
        w.attach(first.writer.clone()).await.unwrap();
        let s = w.attached().await;

        w.detach();
        tokio::time::timeout(Duration::from_secs(1), w.lost(s))
            .await
            .expect("a detach ends the session");

        let second = session().await;
        w.attach(second.writer.clone()).await.unwrap();
        assert_eq!(w.attached().await, s + 1);
    }
}
