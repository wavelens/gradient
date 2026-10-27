/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredJob {
    pub job_id: String,
    pub assignment_id: String,
    pub attempt: u32,
    pub timeout_secs: Option<u64>,
    pub max_silent_secs: Option<u64>,
    pub started_at_unix: u64,
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gradient_vm::{GuestId, GuestState};

    use super::*;

    #[test]
    fn a_stored_job_round_trips_through_the_guest_state() {
        let stored = StoredJob {
            job_id: "job-1".into(),
            assignment_id: "assignment-1".into(),
            attempt: 2,
            timeout_secs: Some(3600),
            max_silent_secs: None,
            started_at_unix: 1_790_000_000,
        };
        let guest_id = GuestId::new();
        let state = GuestState {
            unit: format!("gradient-guest-{guest_id}.service"),
            guest_id,
            cid: 3,
            api_socket: PathBuf::from("/var/lib/gradient-worker/guests/g/api.sock"),
            address: None,
            tap: None,
            job: Some(serde_json::to_value(&stored).unwrap()),
            devices_held: Vec::new(),
            mounts: Vec::new(),
            relays: Vec::new(),
            usb_bus_ids: Vec::new(),
            per_job_dirs: Vec::new(),
        };

        let read = state.job_as::<StoredJob>().unwrap().unwrap();
        assert_eq!(read, stored);
    }
}
