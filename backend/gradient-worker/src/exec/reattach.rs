/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use gradient_vm::{GUEST_BOOT_TIMEOUT, Guest, GuestState, GuestUnits, ReattachedGuest, wait_ready};
use tokio::time::Instant;
use tracing::{info, warn};

use super::agent_client::AgentClient;
use super::host::ExecHost;
use super::job::{finish, unix_now, unless_aborted};
use super::state::StoredJob;
use super::watchdog::Watchdog;
use crate::executor::AbortSignal;
use crate::executor::failure::BuildError;
use crate::proto::job::JobUpdater;

pub struct ResumedJob {
    pub guest: ReattachedGuest,
    pub stored: StoredJob,
}

pub async fn resumable(guests: Vec<ReattachedGuest>, units: &GuestUnits) -> Vec<ResumedJob> {
    let mut resumed = Vec::new();
    for reattached in guests {
        match resumable_job(reattached.guest.state()) {
            Ok(stored) => {
                info!(job_id = %stored.job_id, guest = %reattached.guest.id(), "resuming an exec job in its guest");
                resumed.push(ResumedJob {
                    guest: reattached,
                    stored,
                });
            }
            Err(e) => {
                warn!(guest = %reattached.guest.id(), error = %e, "stopping a busy guest whose job cannot resume");
                reattached.guest.teardown(units).await;
            }
        }
    }
    resumed
}

pub async fn resume(
    host: &ExecHost,
    resumed: ResumedJob,
    updater: &mut JobUpdater,
    abort: AbortSignal,
) -> Result<()> {
    let ResumedJob {
        guest: ReattachedGuest { guest, devices },
        stored,
    } = resumed;
    let outcome = follow_again(host, &guest, &stored, updater, abort).await;
    host.pool.retire(guest, devices);
    outcome
}

fn resumable_job(state: &GuestState) -> Result<StoredJob> {
    let stored = state
        .job_as::<StoredJob>()
        .ok_or_else(|| anyhow!("the guest holds no job record"))??;
    if !stored.sent_to_agent {
        bail!("the worker stopped before the guest agent got the job");
    }
    Ok(stored)
}

async fn follow_again(
    host: &ExecHost,
    guest: &Guest,
    stored: &StoredJob,
    updater: &mut JobUpdater,
    mut abort: AbortSignal,
) -> Result<()> {
    let ready = unless_aborted(
        &abort,
        wait_ready(&guest.vsock_socket(), GUEST_BOOT_TIMEOUT),
    )
    .await?
    .map_err(BuildError::transient)?;
    let mut agent = AgentClient::from(ready);
    agent
        .attach(updater.log.accepted())
        .await
        .map_err(BuildError::transient)?;
    let mut watchdog = resumed_watchdog(stored);
    let leave_running = host.worker_stop_keeping_guests();
    finish(
        &mut agent,
        updater,
        &mut watchdog,
        &mut abort,
        &leave_running,
    )
    .await
}

fn resumed_watchdog(stored: &StoredJob) -> Watchdog {
    let running_for = Duration::from_secs(unix_now().saturating_sub(stored.started_at_unix));
    let now = Instant::now();
    let mut watchdog = Watchdog::new(
        now.checked_sub(running_for).unwrap_or(now),
        stored.timeout_secs.map(Duration::from_secs),
        stored.max_silent_secs.map(Duration::from_secs),
    );
    watchdog.saw_output();
    watchdog
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use gradient_vm::GuestId;

    use crate::exec::watchdog::Watched;

    use super::*;

    fn guest_state(job: Option<serde_json::Value>) -> GuestState {
        let guest_id = GuestId::new();
        GuestState {
            unit: format!("gradient-guest-{guest_id}.service"),
            guest_id,
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

    fn stored(sent_to_agent: bool) -> StoredJob {
        StoredJob {
            job_id: "exec:1".into(),
            assignment_id: "dispatch-1".into(),
            attempt: 1,
            timeout_secs: None,
            max_silent_secs: Some(60),
            started_at_unix: 1_790_000_000,
            sent_to_agent,
        }
    }

    #[test]
    fn only_a_job_the_agent_received_resumes() {
        let sent = guest_state(Some(serde_json::to_value(stored(true)).unwrap()));
        let unsent = guest_state(Some(serde_json::to_value(stored(false)).unwrap()));
        let garbled = guest_state(Some(serde_json::json!({ "job": 1 })));

        assert_eq!(resumable_job(&sent).unwrap(), stored(true));
        assert!(resumable_job(&unsent).is_err());
        assert!(resumable_job(&garbled).is_err());
    }

    #[tokio::test(start_paused = true)]
    async fn a_resumed_job_keeps_its_wall_clock_deadline() {
        let job = StoredJob {
            started_at_unix: unix_now() - 100,
            timeout_secs: Some(130),
            max_silent_secs: None,
            ..stored(true)
        };
        let started = Instant::now();
        let mut watchdog = resumed_watchdog(&job);

        let event = watchdog
            .next(
                &mut futures::stream::pending::<()>(),
                &mut AbortSignal::never(),
            )
            .await;

        assert_eq!(event, Watched::TotalExceeded(Duration::from_secs(130)));
        assert!((29..=31).contains(&started.elapsed().as_secs()));
    }
}
