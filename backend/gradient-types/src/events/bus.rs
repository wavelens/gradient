/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{Envelope, Event};
use std::sync::Arc;
use tokio::sync::broadcast::{self, error::RecvError, error::TryRecvError};

pub const EVENT_BUS_CAPACITY: usize = 4096;

/// Two lanes: proto traffic rides its own, so a NAR push storm cannot lag the
/// live UI sockets; only the firehose listens to both.
#[derive(Clone, Debug)]
pub struct EventBus {
    events: broadcast::Sender<Arc<Envelope>>,
    wire: broadcast::Sender<Arc<Envelope>>,
}

pub struct EventRx {
    events: broadcast::Receiver<Arc<Envelope>>,
    wire: Option<broadcast::Receiver<Arc<Envelope>>>,
}

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        Self {
            events: broadcast::channel(capacity).0,
            wire: broadcast::channel(capacity).0,
        }
    }

    pub fn publish(&self, event: impl Into<Event>) {
        self.publish_envelope(Arc::new(Envelope::now(event.into())));
    }

    pub fn publish_envelope(&self, envelope: Arc<Envelope>) {
        let lane = match envelope.event {
            Event::Proto(_) => &self.wire,
            _ => &self.events,
        };
        let _ = lane.send(envelope);
    }

    /// Whether anyone listens to proto traffic; the tap skips building events otherwise.
    pub fn wire_active(&self) -> bool {
        self.wire.receiver_count() > 0
    }

    pub fn subscribe(&self) -> EventRx {
        EventRx {
            events: self.events.subscribe(),
            wire: None,
        }
    }

    pub fn subscribe_firehose(&self) -> EventRx {
        EventRx {
            events: self.events.subscribe(),
            wire: Some(self.wire.subscribe()),
        }
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(EVENT_BUS_CAPACITY)
    }
}

impl EventRx {
    pub async fn recv(&mut self) -> Result<Arc<Envelope>, RecvError> {
        match &mut self.wire {
            None => self.events.recv().await,
            Some(wire) => tokio::select! {
                biased;
                env = self.events.recv() => env,
                env = wire.recv() => env,
            },
        }
    }

    pub fn try_recv(&mut self) -> Result<Arc<Envelope>, TryRecvError> {
        match self.events.try_recv() {
            Err(TryRecvError::Empty) => match &mut self.wire {
                Some(wire) => wire.try_recv(),
                None => Err(TryRecvError::Empty),
            },
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::proto::{Direction, Message};
    use crate::events::worker::QueueDepth;

    fn proto() -> Message {
        Message {
            direction: Direction::Client,
            worker_id: "w".into(),
            message: "NarPush".into(),
            job_id: None,
            len: Some(1),
        }
    }

    fn depth() -> QueueDepth {
        QueueDepth {
            workers: 1,
            pending: 0,
            active: 0,
        }
    }

    #[test]
    fn proto_traffic_never_reaches_live_subscribers() {
        let bus = EventBus::new(4);
        let mut live = bus.subscribe();
        let _firehose = bus.subscribe_firehose();
        for _ in 0..16 {
            bus.publish(proto());
        }
        bus.publish(depth());
        assert_eq!(live.try_recv().unwrap().event.name(), "worker.queue_depth");
    }

    #[test]
    fn the_firehose_sees_both_lanes() {
        let bus = EventBus::new(4);
        let mut firehose = bus.subscribe_firehose();
        bus.publish(proto());
        bus.publish(depth());
        let names: Vec<_> = std::iter::from_fn(|| firehose.try_recv().ok())
            .map(|env| env.event.name().into_owned())
            .collect();
        assert!(names.contains(&"proto.client.nar_push".to_owned()));
        assert!(names.contains(&"worker.queue_depth".to_owned()));
    }

    #[test]
    fn the_wire_lane_is_idle_without_a_firehose() {
        let bus = EventBus::new(4);
        assert!(!bus.wire_active());
        let _firehose = bus.subscribe_firehose();
        assert!(bus.wire_active());
    }
}
