/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_vm::VmStateChange;
use gradient_wire::messages::{ClientMessage, VmStateKind};
use tokio::sync::broadcast::{self, error::RecvError};

pub(super) fn report_for_session(
    change: VmStateChange,
    runs: impl Fn(&str) -> bool,
) -> Option<ClientMessage> {
    let owned = change.job_id.as_deref().is_some_and(&runs);
    if change.state == VmStateKind::Running && !owned {
        return None;
    }
    Some(ClientMessage::VmState {
        vm: change.vm.to_string(),
        state: change.state,
        job_id: change.job_id.filter(|_| owned),
    })
}

pub(super) async fn next_change(
    changes: &mut Option<broadcast::Receiver<VmStateChange>>,
) -> VmStateChange {
    let Some(receiver) = changes else {
        return std::future::pending().await;
    };
    loop {
        match receiver.recv().await {
            Ok(change) => return change,
            Err(RecvError::Lagged(_)) => continue,
            Err(RecvError::Closed) => {
                *changes = None;
                return std::future::pending().await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_vm::GuestId;

    fn change(state: VmStateKind, job_id: Option<&str>) -> VmStateChange {
        VmStateChange {
            vm: GuestId::new(),
            state,
            job_id: job_id.map(str::to_owned),
        }
    }

    fn job_of(report: Option<ClientMessage>) -> Option<Option<String>> {
        match report? {
            ClientMessage::VmState { job_id, .. } => Some(job_id),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn a_warm_vm_reaches_every_session() {
        assert_eq!(
            job_of(report_for_session(change(VmStateKind::Warm, None), |_| {
                false
            })),
            Some(None)
        );
    }

    #[test]
    fn a_running_vm_reaches_only_the_session_running_its_job() {
        let running = || change(VmStateKind::Running, Some("exec:j1"));
        assert_eq!(
            job_of(report_for_session(running(), |job| job == "exec:j1")),
            Some(Some("exec:j1".into()))
        );
        assert_eq!(job_of(report_for_session(running(), |_| false)), None);
    }

    #[test]
    fn a_stop_reaches_every_session_but_names_the_job_only_to_its_owner() {
        let stopped = || change(VmStateKind::Stopped, Some("exec:j1"));
        assert_eq!(job_of(report_for_session(stopped(), |_| false)), Some(None));
        assert_eq!(
            job_of(report_for_session(stopped(), |job| job == "exec:j1")),
            Some(Some("exec:j1".into()))
        );
    }
}
