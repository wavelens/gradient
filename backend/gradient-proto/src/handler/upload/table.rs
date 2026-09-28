/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gradient_storage::PartialWriter;
use gradient_storage::admission::UploadPermit;
use gradient_wire::types::UploadObject;

pub(in crate::handler) enum Transfer {
    Relay(Box<PartialWriter>),
    Put,
    Multipart { upload_id: String },
}

pub(in crate::handler) enum Lease {
    Idle { last: Instant, idle: Duration },
    Until(Instant),
}

impl Lease {
    fn expired(&self, now: Instant) -> bool {
        match self {
            Lease::Idle { last, idle } => now.duration_since(*last) > *idle,
            Lease::Until(deadline) => now > *deadline,
        }
    }

    pub(in crate::handler) fn touch(&mut self, now: Instant) {
        if let Lease::Idle { last, .. } = self {
            *last = now;
        }
    }
}

pub(in crate::handler) struct Queued {
    pub job_id: String,
    pub object: UploadObject,
    pub size: u64,
}

pub(in crate::handler) struct Granted {
    pub job_id: String,
    pub object: UploadObject,
    pub size: u64,
    #[expect(dead_code, reason = "settled by the commit")]
    pub permit: UploadPermit,
    pub transfer: Transfer,
    pub lease: Lease,
}

enum Entry {
    Queued(Queued),
    Granted(Box<Granted>),
}

impl Entry {
    fn job_id(&self) -> &str {
        match self {
            Entry::Queued(q) => &q.job_id,
            Entry::Granted(g) => &g.job_id,
        }
    }
}

/// Every upload this session asked for and has not settled yet.
#[derive(Default)]
pub(in crate::handler) struct UploadTable {
    entries: HashMap<u64, Entry>,
}

impl UploadTable {
    pub(in crate::handler) fn queue(
        &mut self,
        id: u64,
        job_id: String,
        object: UploadObject,
        size: u64,
    ) -> bool {
        if self.entries.contains_key(&id) {
            return false;
        }
        self.entries.insert(
            id,
            Entry::Queued(Queued {
                job_id,
                object,
                size,
            }),
        );
        true
    }

    pub(in crate::handler) fn take_queued(&mut self, id: u64) -> Option<Queued> {
        match self.entries.remove(&id)? {
            Entry::Queued(q) => Some(q),
            granted => {
                self.entries.insert(id, granted);
                None
            }
        }
    }

    pub(in crate::handler) fn grant(&mut self, id: u64, granted: Granted) {
        self.entries.insert(id, Entry::Granted(Box::new(granted)));
    }

    pub(in crate::handler) fn granted_mut(&mut self, id: u64) -> Option<&mut Granted> {
        match self.entries.get_mut(&id)? {
            Entry::Granted(g) => Some(g),
            Entry::Queued(_) => None,
        }
    }

    pub(in crate::handler) fn take_granted(&mut self, id: u64) -> Option<Granted> {
        match self.entries.remove(&id)? {
            Entry::Granted(g) => Some(*g),
            queued => {
                self.entries.insert(id, queued);
                None
            }
        }
    }

    pub(in crate::handler) fn remove(&mut self, id: u64) -> Option<Granted> {
        match self.entries.remove(&id)? {
            Entry::Granted(g) => Some(*g),
            Entry::Queued(_) => None,
        }
    }

    pub(in crate::handler) fn forget_job(&mut self, job_id: &str) -> Vec<u64> {
        let ids: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, e)| e.job_id() == job_id)
            .map(|(id, _)| *id)
            .collect();
        for id in &ids {
            self.entries.remove(id);
        }
        ids
    }

    pub(in crate::handler) fn expired(&mut self, now: Instant) -> Vec<(u64, Granted)> {
        let ids: Vec<u64> = self
            .entries
            .iter()
            .filter(|(_, e)| matches!(e, Entry::Granted(g) if g.lease.expired(now)))
            .map(|(id, _)| *id)
            .collect();
        ids.into_iter()
            .filter_map(|id| self.take_granted(id).map(|g| (id, g)))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_storage::PartialStore;
    use gradient_storage::admission::{Admitted, Limits, ObjectKey, UploadAdmission};
    use std::sync::Arc;
    use std::time::Duration;

    fn nar(i: u8) -> UploadObject {
        UploadObject::Nar {
            store_path: format!(
                "/nix/store/{}-p",
                char::from(b'a' + i).to_string().repeat(32)
            ),
        }
    }

    async fn permit(
        admission: &Arc<UploadAdmission>,
        id: u64,
    ) -> (gradient_storage::admission::AdmissionSession, UploadPermit) {
        let (session, mut rx) = admission.open_session();
        session.request(id, ObjectKey::Nar(id.to_string()), 1);
        let Some(Admitted::Granted { permit, .. }) = rx.recv().await else {
            panic!("granted")
        };
        (session, permit)
    }

    #[tokio::test]
    async fn an_idle_relay_lease_expires_into_retry() {
        let admission = UploadAdmission::new(Limits {
            concurrency: 4,
            bytes: u64::MAX,
        });
        let dir = tempfile::TempDir::new().unwrap();
        let partials = PartialStore::new(dir.path(), Duration::from_secs(3600)).unwrap();
        let writer = partials.open_writer("peer/a", "a", 0, 0).await.unwrap();
        let (_session, permit) = permit(&admission, 1).await;
        let mut table = UploadTable::default();
        let start = Instant::now();
        assert!(table.queue(1, "build:1".into(), nar(0), 10));
        table.grant(
            1,
            Granted {
                job_id: "build:1".into(),
                object: nar(0),
                size: 10,
                permit,
                transfer: Transfer::Relay(Box::new(writer)),
                lease: Lease::Idle {
                    last: start,
                    idle: Duration::from_secs(300),
                },
            },
        );

        assert!(table.expired(start + Duration::from_secs(299)).is_empty());
        let expired = table.expired(start + Duration::from_secs(301));
        assert_eq!(
            expired.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![1]
        );
        drop(expired);
        assert_eq!(
            admission.in_flight(),
            0,
            "an expired lease returns its permit"
        );
    }

    #[tokio::test]
    async fn a_touched_relay_lease_stays_alive() {
        let admission = UploadAdmission::new(Limits {
            concurrency: 4,
            bytes: u64::MAX,
        });
        let (_session, permit) = permit(&admission, 1).await;
        let mut table = UploadTable::default();
        let start = Instant::now();
        table.queue(1, "build:1".into(), nar(0), 10);
        table.grant(
            1,
            Granted {
                job_id: "build:1".into(),
                object: nar(0),
                size: 10,
                permit,
                transfer: Transfer::Put,
                lease: Lease::Idle {
                    last: start,
                    idle: Duration::from_secs(300),
                },
            },
        );
        table
            .granted_mut(1)
            .unwrap()
            .lease
            .touch(start + Duration::from_secs(200));
        assert!(table.expired(start + Duration::from_secs(400)).is_empty());
    }

    #[test]
    fn a_duplicate_request_id_is_refused() {
        let mut table = UploadTable::default();
        assert!(table.queue(1, "build:1".into(), nar(0), 1));
        assert!(!table.queue(1, "build:1".into(), nar(1), 1));
    }

    #[test]
    fn forgetting_a_job_returns_only_its_requests() {
        let mut table = UploadTable::default();
        table.queue(1, "build:1".into(), nar(0), 1);
        table.queue(2, "build:2".into(), nar(1), 1);
        table.queue(3, "build:1".into(), nar(2), 1);
        let mut ids = table.forget_job("build:1");
        ids.sort();
        assert_eq!(ids, vec![1, 3]);
        assert!(table.take_queued(2).is_some());
    }
}
