/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! The pending deliveries are storing each [`Event`] by its Rust variant name. The published
//! [`Envelope`] is carrying its dotted name.

pub mod audit;
pub mod build;
mod bus;
pub mod cache;
mod envelope;
pub mod evaluation;
mod filter;
pub mod gc;
pub mod graph;
pub mod proto;
pub mod webhook;
pub mod worker;

pub use bus::{EVENT_BUS_CAPACITY, EventBus, EventRx};
pub use envelope::Envelope;
pub use filter::EventFilter;

use crate::ids::{CacheId, ProjectId, TaskId};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventOwner {
    pub project: Option<ProjectId>,
    pub task: Option<TaskId>,
    pub cache: Option<CacheId>,
}

pub trait EventKind: Serialize {
    const NAME: &'static str;
    const DURABLE: bool;
    const NAMES: &'static [&'static str] = &[Self::NAME];

    fn name(&self) -> Cow<'static, str> {
        Cow::Borrowed(Self::NAME)
    }

    fn owner(&self) -> EventOwner {
        EventOwner::default()
    }

    fn key(&self) -> Option<String> {
        None
    }

    fn personal(&self) -> bool {
        false
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct CatalogEntry {
    pub name: &'static str,
    pub durable: bool,
}

macro_rules! firehose {
    ($ty:ident, $name:literal) => {
        impl $crate::events::EventKind for $ty {
            const NAME: &'static str = $name;
            const DURABLE: bool = false;
        }
    };
}
pub(crate) use firehose;

macro_rules! events {
    ($($variant:ident($ty:ty)),* $(,)?) => {
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        pub enum Event {
            $($variant($ty)),*
        }

        impl Event {
            pub fn name(&self) -> Cow<'static, str> {
                match self { $(Event::$variant(e) => e.name()),* }
            }

            pub fn durable(&self) -> bool {
                match self { $(Event::$variant(_) => <$ty as EventKind>::DURABLE),* }
            }

            pub fn owner(&self) -> EventOwner {
                match self { $(Event::$variant(e) => e.owner()),* }
            }

            pub fn key(&self) -> Option<String> {
                match self { $(Event::$variant(e) => e.key()),* }
            }

            pub fn personal(&self) -> bool {
                match self { $(Event::$variant(e) => e.personal()),* }
            }

            pub fn content(&self) -> serde_json::Value {
                match self {
                    $(Event::$variant(e) => serde_json::to_value(e).unwrap_or_default()),*
                }
            }

            pub fn catalog() -> Vec<CatalogEntry> {
                let mut entries = Vec::new();
                $(
                    for name in <$ty as EventKind>::NAMES {
                        entries.push(CatalogEntry { name, durable: <$ty as EventKind>::DURABLE });
                    }
                )*
                entries
            }
        }

        $(impl From<$ty> for Event {
            fn from(e: $ty) -> Self {
                Event::$variant(e)
            }
        })*
    };
}

events! {
    BuildStatusChanged(build::StatusChanged),
    BuildReported(build::Reported),
    BuildProgress(build::Progress),
    EvaluationReported(evaluation::Reported),
    EvaluationProgress(evaluation::Progress),
    GraphRecorded(graph::Recorded),
    GraphNarCommitted(graph::NarCommitted),
    GraphTransitioned(graph::Transitioned),
    GraphRequeued(graph::Requeued),
    GraphDemoted(graph::Demoted),
    GraphCollected(graph::Collected),
    WorkerConnected(worker::Connected),
    WorkerDisconnected(worker::Disconnected),
    WorkerJobDispatched(worker::JobDispatched),
    WorkerQueueDepth(worker::QueueDepth),
    Proto(proto::Message),
    CacheChanged(cache::Changed),
    CacheNarFetched(cache::NarFetched),
    CacheNarinfoServed(cache::NarinfoServed),
    CacheNarSigned(cache::NarSigned),
    GcSwept(gc::Swept),
    GcDeepFinished(gc::DeepFinished),
    Audit(audit::Audited),
    WebhookPing(webhook::Ping),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn catalog_names_are_unique() {
        let catalog = Event::catalog();
        let names: HashSet<_> = catalog.iter().map(|e| e.name).collect();
        assert_eq!(names.len(), catalog.len());
    }
}
