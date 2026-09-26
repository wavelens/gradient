/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{Envelope, Event};
use std::sync::Arc;
use tokio::sync::broadcast;

pub const EVENT_BUS_CAPACITY: usize = 4096;

pub type EventRx = broadcast::Receiver<Arc<Envelope>>;

#[derive(Clone, Debug)]
pub struct EventBus(broadcast::Sender<Arc<Envelope>>);

impl EventBus {
    pub fn new(capacity: usize) -> Self {
        Self(broadcast::channel(capacity).0)
    }

    pub fn publish(&self, event: impl Into<Event>) {
        self.publish_envelope(Arc::new(Envelope::now(event.into())));
    }

    pub fn publish_envelope(&self, envelope: Arc<Envelope>) {
        let _ = self.0.send(envelope);
    }

    pub fn subscribe(&self) -> EventRx {
        self.0.subscribe()
    }
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new(EVENT_BUS_CAPACITY)
    }
}
