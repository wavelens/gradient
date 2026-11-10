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
pub mod exec_job;
mod filter;
pub mod gc;
pub mod graph;
pub mod names;
pub mod proto;
pub mod vm;
pub mod webhook;
pub mod worker;
pub mod worker_demand;

pub use bus::{EVENT_BUS_CAPACITY, EventBus, EventRx};
pub use envelope::Envelope;
pub use filter::EventFilter;

use crate::ids::{CacheId, ProjectId, TaskId, TeamId};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventOwner {
    pub project: Option<ProjectId>,
    pub task: Option<TaskId>,
    pub cache: Option<CacheId>,
    pub team: Option<TeamId>,
}

impl EventOwner {
    pub fn project(project: ProjectId) -> Self {
        Self {
            project: Some(project),
            ..Self::default()
        }
    }

    pub fn task(project: ProjectId, task: TaskId) -> Self {
        Self {
            project: Some(project),
            task: Some(task),
            ..Self::default()
        }
    }

    pub fn project_cache(project: ProjectId, cache: CacheId) -> Self {
        Self {
            project: Some(project),
            cache: Some(cache),
            ..Self::default()
        }
    }

    pub fn cache(cache: CacheId) -> Self {
        Self {
            cache: Some(cache),
            ..Self::default()
        }
    }

    pub fn team(team: TeamId) -> Self {
        Self {
            team: Some(team),
            ..Self::default()
        }
    }
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

    fn status_name(&self) -> Option<serde_json::Value> {
        None
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, utoipa::ToSchema)]
pub struct CatalogEntry {
    pub name: &'static str,
    pub durable: bool,
}

macro_rules! firehose {
    ($ty:ident, $name:expr) => {
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
                    $(Event::$variant(e) => {
                        let mut content = serde_json::to_value(e).unwrap_or_default();
                        if let (Some(status_name), Some(fields)) =
                            (e.status_name(), content.as_object_mut())
                        {
                            fields.insert("status_name".into(), status_name);
                        }
                        content
                    }),*
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
    ExecJobReported(exec_job::Reported),
    EvaluationReported(evaluation::Reported),
    EvaluationProgress(evaluation::Progress),
    EvaluationActivity(evaluation::Activity),
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
    WorkerDemand(worker_demand::WorkerDemand),
    Proto(proto::Message),
    CacheChanged(cache::Changed),
    CacheNarFetched(cache::NarFetched),
    CacheNarinfoServed(cache::NarinfoServed),
    CacheNarSigned(cache::NarSigned),
    GcSwept(gc::Swept),
    GcDeepFinished(gc::DeepFinished),
    Audit(audit::Audited),
    WebhookPing(webhook::Ping),
    VmStateChanged(vm::StateChanged),
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

    #[test]
    fn job_status_event_json_has_readable_status_name() {
        use gradient_entity::{
            build::BuildStatus, evaluation::EvaluationStatus, exec_job::ExecJobStatus,
        };

        let build = Event::from(build::Reported {
            status: i16::from(BuildStatus::FailedPermanent),
            ..Default::default()
        });
        let changed = Event::from(build::StatusChanged {
            build_id: Default::default(),
            derivation_build: Default::default(),
            evaluation_id: Default::default(),
            status: i16::from(BuildStatus::Queued),
        });
        let evaluation = Event::from(evaluation::Reported {
            status: i16::from(EvaluationStatus::Building),
            ..Default::default()
        });
        let exec_job = Event::from(exec_job::Reported {
            status: i16::from(ExecJobStatus::Running),
            ..Default::default()
        });

        assert_eq!(build.content()["status"], 4);
        assert_eq!(build.content()["status_name"], "FailedPermanent");
        assert_eq!(changed.content()["status_name"], "Queued");
        assert_eq!(evaluation.content()["status_name"], "Building");
        assert_eq!(exec_job.content()["status_name"], "running");
    }

    #[test]
    fn event_with_unknown_or_missing_status_has_no_status_name_in_json_content() {
        let unknown = Event::from(build::Reported {
            status: 99,
            ..Default::default()
        });
        let without_status = Event::from(cache::Changed {});

        assert!(unknown.content().get("status_name").is_none());
        assert!(without_status.content().get("status_name").is_none());
    }
}
