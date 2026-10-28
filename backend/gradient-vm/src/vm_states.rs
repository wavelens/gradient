/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use gradient_wire::messages::VmStateKind;
use tokio::sync::broadcast;

use crate::state::GuestId;

const CHANGES_BUFFERED: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmStateChange {
    pub vm: GuestId,
    pub state: VmStateKind,
    pub job_id: Option<String>,
}

pub struct VmStates {
    live: Mutex<BTreeMap<GuestId, VmStateChange>>,
    changes: broadcast::Sender<VmStateChange>,
}

impl Default for VmStates {
    fn default() -> Self {
        Self {
            live: Mutex::default(),
            changes: broadcast::channel(CHANGES_BUFFERED).0,
        }
    }
}

impl VmStates {
    pub fn warm(&self, vm: &GuestId) {
        self.record(vm, VmStateKind::Warm, None);
    }

    pub fn running(&self, vm: &GuestId, job_id: String) {
        self.record(vm, VmStateKind::Running, Some(job_id));
    }

    pub fn stopped(&self, vm: &GuestId) {
        let Some(last) = self.lock().remove(vm) else {
            return;
        };
        let _ = self.changes.send(VmStateChange {
            state: VmStateKind::Stopped,
            ..last
        });
    }

    pub fn subscribe(&self) -> broadcast::Receiver<VmStateChange> {
        self.changes.subscribe()
    }

    pub fn live(&self) -> Vec<VmStateChange> {
        self.lock().values().cloned().collect()
    }

    fn record(&self, vm: &GuestId, state: VmStateKind, job_id: Option<String>) {
        let change = VmStateChange {
            vm: vm.clone(),
            state,
            job_id,
        };
        self.lock().insert(vm.clone(), change.clone());
        let _ = self.changes.send(change);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<GuestId, VmStateChange>> {
        self.live.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stop_of_a_running_vm_names_its_job_and_leaves_the_live_states() {
        let states = VmStates::default();
        let vm = GuestId::new();
        let mut changes = states.subscribe();
        states.warm(&vm);
        states.running(&vm, "exec:j1".into());
        states.stopped(&vm);

        assert!(states.live().is_empty());
        let seen: Vec<_> = std::iter::from_fn(|| changes.try_recv().ok())
            .map(|change| (change.state, change.job_id))
            .collect();
        assert_eq!(
            seen,
            vec![
                (VmStateKind::Warm, None),
                (VmStateKind::Running, Some("exec:j1".into())),
                (VmStateKind::Stopped, Some("exec:j1".into())),
            ]
        );
    }

    #[test]
    fn the_stop_of_an_unseen_vm_is_not_reported() {
        let states = VmStates::default();
        let mut changes = states.subscribe();
        states.stopped(&GuestId::new());
        assert!(changes.try_recv().is_err());
    }
}
