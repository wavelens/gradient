/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use gradient_util::sync::Mutex;
use gradient_wire::messages::GUEST_CHANNEL_CHUNK;
use gradient_wire::types::GuestChannelKind;
use tokio::sync::mpsc;

use crate::session_port::{SessionPort, SessionSignal};

pub const GUEST_CHANNEL_BUFFER: usize = 256;

const OVERFLOW_MESSAGE: &str = "the terminal could not keep up";
const DISCONNECT_MESSAGE: &str = "the worker disconnected";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestChannelSignal {
    Open {
        job_id: String,
        channel_id: u64,
        kind: GuestChannelKind,
    },
    Data {
        channel_id: u64,
        data: Vec<u8>,
    },
    Resize {
        channel_id: u64,
        cols: u16,
        rows: u16,
    },
    Close {
        channel_id: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestChannelEvent {
    Data(Vec<u8>),
    Closed {
        exit_code: Option<i32>,
        message: Option<String>,
    },
}

struct Route {
    worker: String,
    port: Arc<dyn SessionPort>,
    events: mpsc::Sender<GuestChannelEvent>,
}

impl Route {
    fn has_room_for_data(&self) -> bool {
        self.events.capacity() > 1
    }

    fn signal(&self, signal: GuestChannelSignal) {
        self.port.signal(SessionSignal::GuestChannel(signal));
    }

    fn close(&self, exit_code: Option<i32>, message: Option<String>) {
        let _ = self
            .events
            .try_send(GuestChannelEvent::Closed { exit_code, message });
    }
}

pub struct GuestChannels {
    next_id: AtomicU64,
    open: Mutex<HashMap<u64, Route>>,
}

impl std::fmt::Debug for GuestChannels {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GuestChannels")
            .field("open", &self.open.lock().len())
            .finish_non_exhaustive()
    }
}

impl GuestChannels {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            next_id: AtomicU64::new(1),
            open: Mutex::new(HashMap::new()),
        })
    }

    pub fn open(
        self: &Arc<Self>,
        worker: &str,
        port: Arc<dyn SessionPort>,
        job_id: String,
        kind: GuestChannelKind,
    ) -> (GuestChannelHandle, mpsc::Receiver<GuestChannelEvent>) {
        let channel_id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (events, receiver) = mpsc::channel(GUEST_CHANNEL_BUFFER + 1);
        let route = Route {
            worker: worker.to_owned(),
            port,
            events,
        };
        let mut open = self.open.lock();
        route.signal(GuestChannelSignal::Open {
            job_id,
            channel_id,
            kind,
        });
        open.insert(channel_id, route);
        drop(open);
        let handle = GuestChannelHandle(Arc::new(HandleInner {
            channel_id,
            channels: Arc::clone(self),
        }));
        (handle, receiver)
    }

    pub fn deliver(&self, worker: &str, channel_id: u64, data: Vec<u8>) {
        let mut open = self.open.lock();
        let Some(route) = open.get(&channel_id).filter(|r| r.worker == worker) else {
            return;
        };
        if route.has_room_for_data() {
            let _ = route.events.try_send(GuestChannelEvent::Data(data));
            return;
        }
        if let Some(route) = open.remove(&channel_id) {
            route.close(None, Some(OVERFLOW_MESSAGE.into()));
            route.signal(GuestChannelSignal::Close { channel_id });
        }
    }

    pub fn closed_by_worker(
        &self,
        worker: &str,
        channel_id: u64,
        exit_code: Option<i32>,
        message: Option<String>,
    ) {
        let mut open = self.open.lock();
        if open.get(&channel_id).is_some_and(|r| r.worker == worker)
            && let Some(route) = open.remove(&channel_id)
        {
            route.close(exit_code, message);
        }
    }

    pub fn close_worker(&self, worker: &str) {
        let mut open = self.open.lock();
        open.retain(|_, route| {
            if route.worker != worker {
                return true;
            }
            route.close(None, Some(DISCONNECT_MESSAGE.into()));
            false
        });
    }

    fn signal_if_open(&self, channel_id: u64, signal: GuestChannelSignal) {
        if let Some(route) = self.open.lock().get(&channel_id) {
            route.signal(signal);
        }
    }

    fn release(&self, channel_id: u64) {
        let route = self.open.lock().remove(&channel_id);
        if let Some(route) = route {
            route.signal(GuestChannelSignal::Close { channel_id });
        }
    }
}

struct HandleInner {
    channel_id: u64,
    channels: Arc<GuestChannels>,
}

impl Drop for HandleInner {
    fn drop(&mut self) {
        self.channels.release(self.channel_id);
    }
}

#[derive(Clone)]
pub struct GuestChannelHandle(Arc<HandleInner>);

impl std::fmt::Debug for GuestChannelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("GuestChannelHandle")
            .field(&self.id())
            .finish()
    }
}

impl GuestChannelHandle {
    pub fn id(&self) -> u64 {
        self.0.channel_id
    }

    pub fn send(&self, data: &[u8]) {
        for chunk in data.chunks(GUEST_CHANNEL_CHUNK) {
            self.0.channels.signal_if_open(
                self.id(),
                GuestChannelSignal::Data {
                    channel_id: self.id(),
                    data: chunk.to_vec(),
                },
            );
        }
    }

    pub fn resize(&self, cols: u16, rows: u16) {
        self.0.channels.signal_if_open(
            self.id(),
            GuestChannelSignal::Resize {
                channel_id: self.id(),
                cols,
                rows,
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct RecordingPort(Mutex<Vec<SessionSignal>>);

    impl SessionPort for RecordingPort {
        fn signal(&self, signal: SessionSignal) {
            self.0.lock().push(signal);
        }
    }

    impl RecordingPort {
        fn guest_signals(&self) -> Vec<GuestChannelSignal> {
            self.0
                .lock()
                .iter()
                .filter_map(|s| match s {
                    SessionSignal::GuestChannel(g) => Some(g.clone()),
                    _ => None,
                })
                .collect()
        }

        fn closes(&self, id: u64) -> usize {
            self.guest_signals()
                .iter()
                .filter(|s| **s == GuestChannelSignal::Close { channel_id: id })
                .count()
        }
    }

    fn open_on(
        channels: &Arc<GuestChannels>,
        worker: &str,
        port: &Arc<RecordingPort>,
    ) -> (GuestChannelHandle, mpsc::Receiver<GuestChannelEvent>) {
        let port: Arc<dyn SessionPort> = Arc::clone(port) as Arc<dyn SessionPort>;
        channels.open(worker, port, "exec:j".into(), GuestChannelKind::Console)
    }

    #[test]
    fn an_open_channel_gets_a_fresh_id_and_reaches_the_worker() {
        let channels = GuestChannels::new();
        let port = Arc::new(RecordingPort::default());
        let (a, _ra) = open_on(&channels, "w1", &port);
        let (b, _rb) = open_on(&channels, "w1", &port);
        assert_ne!(a.id(), b.id());
        let opened: Vec<u64> = port
            .guest_signals()
            .into_iter()
            .filter_map(|s| match s {
                GuestChannelSignal::Open {
                    job_id, channel_id, ..
                } if job_id == "exec:j" => Some(channel_id),
                _ => None,
            })
            .collect();
        assert_eq!(opened, vec![a.id(), b.id()]);
    }

    #[test]
    fn worker_data_reaches_the_opener_and_dropping_the_handle_closes_it() {
        let channels = GuestChannels::new();
        let port = Arc::new(RecordingPort::default());
        let (handle, mut events) = open_on(&channels, "w1", &port);
        let id = handle.id();
        channels.deliver("w1", id, b"hi".to_vec());
        assert_eq!(
            events.try_recv().unwrap(),
            GuestChannelEvent::Data(b"hi".to_vec())
        );
        drop(handle);
        assert_eq!(port.closes(id), 1);
        channels.deliver("w1", id, b"late".to_vec());
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_close_from_the_worker_ends_the_channel_without_echo() {
        let channels = GuestChannels::new();
        let port = Arc::new(RecordingPort::default());
        let (handle, mut events) = open_on(&channels, "w1", &port);
        let id = handle.id();
        channels.closed_by_worker("w1", id, Some(0), None);
        assert_eq!(
            events.try_recv().unwrap(),
            GuestChannelEvent::Closed {
                exit_code: Some(0),
                message: None
            }
        );
        drop(handle);
        assert_eq!(port.closes(id), 0);
    }

    #[test]
    fn data_from_another_worker_is_dropped() {
        let channels = GuestChannels::new();
        let port = Arc::new(RecordingPort::default());
        let (handle, mut events) = open_on(&channels, "w1", &port);
        channels.deliver("w2", handle.id(), b"spoof".to_vec());
        channels.closed_by_worker("w2", handle.id(), Some(1), None);
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn a_session_end_closes_its_worker_channels_only() {
        let channels = GuestChannels::new();
        let port = Arc::new(RecordingPort::default());
        let (_one, mut on_w1) = open_on(&channels, "w1", &port);
        let (_two, mut on_w2) = open_on(&channels, "w2", &port);
        channels.close_worker("w1");
        assert_eq!(
            on_w1.try_recv().unwrap(),
            GuestChannelEvent::Closed {
                exit_code: None,
                message: Some("the worker disconnected".into())
            }
        );
        assert!(on_w2.try_recv().is_err());
    }

    #[test]
    fn a_full_buffer_closes_the_channel_instead_of_blocking() {
        let channels = GuestChannels::new();
        let port = Arc::new(RecordingPort::default());
        let (handle, mut events) = open_on(&channels, "w1", &port);
        for _ in 0..=GUEST_CHANNEL_BUFFER {
            channels.deliver("w1", handle.id(), b"x".to_vec());
        }
        assert_eq!(port.closes(handle.id()), 1);
        let mut last = None;
        while let Ok(event) = events.try_recv() {
            last = Some(event);
        }
        assert_eq!(
            last,
            Some(GuestChannelEvent::Closed {
                exit_code: None,
                message: Some("the terminal could not keep up".into())
            })
        );
    }

    #[test]
    fn send_splits_large_writes() {
        let channels = GuestChannels::new();
        let port = Arc::new(RecordingPort::default());
        let (handle, _events) = open_on(&channels, "w1", &port);
        handle.send(&vec![0; GUEST_CHANNEL_CHUNK + 1]);
        let sizes: Vec<usize> = port
            .guest_signals()
            .into_iter()
            .filter_map(|s| match s {
                GuestChannelSignal::Data { data, .. } => Some(data.len()),
                _ => None,
            })
            .collect();
        assert_eq!(sizes, vec![GUEST_CHANNEL_CHUNK, 1]);
    }
}
