/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::scheduling::assignment_record::exec_job_of_key;
use gradient_types::events::vm::{StateChanged, VmState};
use gradient_wire::messages::VmStateKind;

pub(super) fn vm_state_changed(
    worker_id: &str,
    vm: String,
    state: VmStateKind,
    job_id: Option<&str>,
) -> StateChanged {
    StateChanged {
        worker_id: worker_id.to_owned(),
        vm,
        state: match state {
            VmStateKind::Warm => VmState::Warm,
            VmStateKind::Running => VmState::Running,
            VmStateKind::Stopped => VmState::Stopped,
        },
        exec_job: job_id.and_then(exec_job_of_key),
    }
}
