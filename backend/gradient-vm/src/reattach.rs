/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::Result;
use tracing::{info, warn};

use crate::devices::{DeviceHold, DeviceInventory};
use crate::guest::{Guest, remove_guest_dirs};
use crate::pool::GuestPool;
use crate::state::{GuestId, GuestState};

#[derive(Debug, Default)]
pub struct ReattachPlan {
    pub warm: Vec<GuestState>,
    pub busy: Vec<GuestState>,
    pub lost: Vec<GuestState>,
    pub orphans: Vec<GuestId>,
}

pub struct ReattachedGuest {
    pub guest: Guest,
    pub devices: DeviceHold,
}

pub fn plan_reattach(
    units: &[(GuestId, bool)],
    states: Vec<(GuestId, Result<GuestState>)>,
) -> ReattachPlan {
    let active: BTreeSet<&GuestId> = units
        .iter()
        .filter(|(_, active)| *active)
        .map(|(id, _)| id)
        .collect();
    let mut plan = ReattachPlan::default();
    let mut readable = BTreeSet::new();
    for (id, state) in states {
        let state = match state {
            Ok(state) => state,
            Err(e) => {
                warn!(guest = %id, error = %e, "guest state unreadable");
                plan.orphans.push(id);
                continue;
            }
        };
        readable.insert(id.clone());
        match (active.contains(&id), state.job.is_some()) {
            (false, _) => plan.lost.push(state),
            (true, true) => plan.busy.push(state),
            (true, false) => plan.warm.push(state),
        }
    }
    plan.orphans.extend(
        units
            .iter()
            .map(|(id, _)| id)
            .filter(|id| !readable.contains(*id) && !plan.orphans.contains(*id))
            .cloned()
            .collect::<Vec<_>>(),
    );
    plan
}

pub async fn apply(
    plan: ReattachPlan,
    pool: &Arc<GuestPool>,
    devices: &DeviceInventory,
) -> Vec<ReattachedGuest> {
    let config = pool.config();
    for id in plan.orphans {
        let stopped = match pool.units().stop(&id).await {
            Ok(()) => true,
            Err(e) => {
                warn!(guest = %id, error = %e, "could not stop a guest unit without state");
                false
            }
        };
        remove_guest_dirs(&config.guests_dir.join(id.to_string()), &[], stopped);
    }
    for state in plan.lost {
        info!(guest = %state.guest_id, "guest unit ended while the worker was down");
        let dir = config.guests_dir.join(state.guest_id.to_string());
        remove_guest_dirs(&dir, &state.per_job_dirs, true);
    }
    for state in plan.warm {
        let guest = Guest::from_state(config, state);
        match guest.is_paused().await {
            true => pool.adopt_warm(guest),
            false => {
                info!(guest = %guest.id(), "stopping a guest that never finished its boot");
                pool.retire(guest, ());
            }
        }
    }
    plan.busy
        .into_iter()
        .map(|state| ReattachedGuest {
            devices: devices.adopt(&state.devices_held),
            guest: Guest::from_state(config, state),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use anyhow::anyhow;

    use super::*;

    fn state(id: &GuestId, job: Option<serde_json::Value>) -> GuestState {
        GuestState {
            guest_id: id.clone(),
            unit: format!("gradient-guest-{id}.service"),
            cid: 3,
            api_socket: PathBuf::from("api.sock"),
            address: None,
            tap: None,
            job,
            devices_held: Vec::new(),
            mounts: Vec::new(),
            relays: Vec::new(),
            usb_bus_ids: Vec::new(),
            per_job_dirs: Vec::new(),
        }
    }

    #[test]
    fn restart_sorts_guests_by_unit_and_state() {
        let [busy, warm, lost, orphan, stray] = std::array::from_fn(|_| GuestId::new());
        let units = [
            (busy.clone(), true),
            (warm.clone(), true),
            (lost.clone(), false),
            (orphan.clone(), true),
        ];
        let states = vec![
            (
                busy.clone(),
                Ok(state(&busy, Some(serde_json::json!({ "job_id": "j1" })))),
            ),
            (warm.clone(), Ok(state(&warm, None))),
            (lost.clone(), Ok(state(&lost, None))),
            (orphan.clone(), Err(anyhow!("no state.json"))),
            (stray.clone(), Err(anyhow!("no state.json"))),
        ];

        let plan = plan_reattach(&units, states);

        let ids = |states: &[GuestState]| -> Vec<GuestId> {
            states.iter().map(|state| state.guest_id.clone()).collect()
        };
        assert_eq!(ids(&plan.busy), [busy]);
        assert_eq!(ids(&plan.warm), [warm]);
        assert_eq!(ids(&plan.lost), [lost]);
        let mut orphans = plan.orphans;
        orphans.sort();
        let mut expected = vec![orphan, stray];
        expected.sort();
        assert_eq!(orphans, expected);
    }
}
