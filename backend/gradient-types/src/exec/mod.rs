/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod continue_file;
mod job_token;

pub use continue_file::{ContinueFile, ContinueFileError, MAX_CONTINUE_FILE_BYTES};
pub use job_token::{JOB_TOKEN_PREFIX, hash_job_token, job_token_expiry, mint_job_token};

pub use gradient_wire::types::{
    DeviceClass, DeviceRequest, DeviceRequestError, ExecAllow, ExecDefaults, ExecInventory,
    ExecLimits, ExecRequirement, ExecSpec, GuestCapacity, GuestSize, Phase, ResourceAmount,
    SecretNameError, SopsFile, WarmGuests, count_warm_guests, exec_image_attr,
    validate_secret_name,
};

pub const DISABLED_ON_INSTANCE: &str = "exec jobs are disabled on this instance";
pub const DISABLED_FOR_TASK: &str = "exec jobs are disabled for this task";

pub const DISABLED_FOR_BUILD_REQUESTS: &str = "Exec jobs don't run in build requests for this project. Enable \"Allow Exec Jobs in Build-Requests\" in project settings.";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskExecSwitch {
    Task { enabled: bool },
    BuildRequests { allowed: bool },
}

impl TaskExecSwitch {
    fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Task { enabled: false } => Some(DISABLED_FOR_TASK),
            Self::BuildRequests { allowed: false } => Some(DISABLED_FOR_BUILD_REQUESTS),
            _ => None,
        }
    }
}

pub fn exec_refusal(instance_enabled: bool, task: TaskExecSwitch) -> Option<&'static str> {
    if !instance_enabled {
        Some(DISABLED_ON_INSTANCE)
    } else {
        task.refusal()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instance_switch_is_checked_before_the_task_switch() {
        let off = TaskExecSwitch::Task { enabled: false };
        assert_eq!(exec_refusal(false, off), Some(DISABLED_ON_INSTANCE));
        assert_eq!(exec_refusal(true, off), Some(DISABLED_FOR_TASK));
        assert_eq!(
            exec_refusal(true, TaskExecSwitch::Task { enabled: true }),
            None
        );
    }

    #[test]
    fn build_requests_are_refused_with_the_project_checkbox() {
        assert_eq!(
            exec_refusal(true, TaskExecSwitch::BuildRequests { allowed: false }),
            Some(DISABLED_FOR_BUILD_REQUESTS)
        );
        assert_eq!(
            exec_refusal(true, TaskExecSwitch::BuildRequests { allowed: true }),
            None
        );
    }
}
