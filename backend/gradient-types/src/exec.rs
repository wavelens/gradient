/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_wire::types::{
    DeviceClass, DeviceRequest, DeviceRequestError, ExecAllow, ExecDefaults, ExecInventory,
    ExecLimits, ExecRequirement, ExecSpec, Phase, SopsFile, exec_image_attr,
};

pub const DISABLED_ON_INSTANCE: &str = "exec jobs are disabled on this instance";
pub const DISABLED_FOR_PROJECT: &str = "exec jobs are disabled for this project";

pub fn exec_refusal(
    instance_enabled: bool,
    project_enabled: bool,
    nodes: u32,
) -> Option<&'static str> {
    if !instance_enabled {
        Some(DISABLED_ON_INSTANCE)
    } else if !project_enabled {
        Some(DISABLED_FOR_PROJECT)
    } else if nodes > 1 {
        Some("exec jobs with more than one node are not supported yet")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_instance_switch_is_checked_before_the_project_switch() {
        assert_eq!(
            exec_refusal(false, false, 1),
            Some("exec jobs are disabled on this instance")
        );
        assert_eq!(
            exec_refusal(true, false, 1),
            Some("exec jobs are disabled for this project")
        );
        assert_eq!(
            exec_refusal(true, true, 2),
            Some("exec jobs with more than one node are not supported yet")
        );
        assert_eq!(exec_refusal(true, true, 1), None);
    }
}
