/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

pub use gradient_wire::types::{
    DeviceClass, DeviceRequest, DeviceRequestError, Phase, RunnerAllow, RunnerDefaults,
    RunnerInventory, RunnerLimits, RunnerRequirement, RunnerSpec, SopsFile, runner_image_attr,
};

pub const DISABLED_ON_INSTANCE: &str = "runner jobs are disabled on this instance";
pub const DISABLED_FOR_PROJECT: &str = "runner jobs are disabled for this project";

pub fn runner_refusal(
    instance_enabled: bool,
    project_enabled: bool,
    nodes: u32,
) -> Option<&'static str> {
    if !instance_enabled {
        Some(DISABLED_ON_INSTANCE)
    } else if !project_enabled {
        Some(DISABLED_FOR_PROJECT)
    } else if nodes > 1 {
        Some("runner jobs with more than one node are not supported yet")
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
            runner_refusal(false, false, 1),
            Some("runner jobs are disabled on this instance")
        );
        assert_eq!(
            runner_refusal(true, false, 1),
            Some("runner jobs are disabled for this project")
        );
        assert_eq!(
            runner_refusal(true, true, 2),
            Some("runner jobs with more than one node are not supported yet")
        );
        assert_eq!(runner_refusal(true, true, 1), None);
    }
}
