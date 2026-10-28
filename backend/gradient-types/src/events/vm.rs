/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::{firehose, names};
use crate::ids::ExecJobId;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmState {
    Warm,
    Running,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateChanged {
    pub worker_id: String,
    pub vm: String,
    pub state: VmState,
    pub exec_job: Option<ExecJobId>,
}

firehose!(StateChanged, names::VM_STATE);
