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
pub const DISABLED_FOR_PROJECT: &str = "exec jobs are disabled for this project";
pub const DISABLED_FOR_TASK: &str = "exec jobs are disabled for this task";

pub fn exec_refusal(
    instance_enabled: bool,
    project_enabled: bool,
    task_enabled: bool,
) -> Option<&'static str> {
    if !instance_enabled {
        Some(DISABLED_ON_INSTANCE)
    } else if !project_enabled {
        Some(DISABLED_FOR_PROJECT)
    } else if !task_enabled {
        Some(DISABLED_FOR_TASK)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switches_are_checked_from_instance_over_project_to_task() {
        assert_eq!(
            exec_refusal(false, false, false),
            Some("exec jobs are disabled on this instance")
        );
        assert_eq!(
            exec_refusal(true, false, false),
            Some("exec jobs are disabled for this project")
        );
        assert_eq!(
            exec_refusal(true, true, false),
            Some("exec jobs are disabled for this task")
        );
        assert_eq!(exec_refusal(true, true, true), None);
    }
}
