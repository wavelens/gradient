/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::Path;

use crate::agent::GuestBootError;

pub const NO_GUEST_AGENT: &str =
    "guest image has no gradient agent, import the gradient.guest module";
const GUEST_AGENT_UNIT: &str = "toplevel/etc/systemd/system/gradient-guest-agent.service";

fn has_guest_agent(image: &Path) -> bool {
    std::fs::canonicalize(image.join(GUEST_AGENT_UNIT))
        .is_ok_and(|unit| unit != Path::new("/dev/null"))
}

pub fn custom_boot_error(error: GuestBootError, image: &Path) -> GuestBootError {
    if has_guest_agent(image) {
        error
    } else {
        GuestBootError(NO_GUEST_AGENT.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_unit(image: &Path) -> std::path::PathBuf {
        let unit = image.join(GUEST_AGENT_UNIT);
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        unit
    }

    #[test]
    fn a_boot_timeout_without_the_agent_unit_names_the_guest_module() {
        let image = tempfile::tempdir().unwrap();
        let timeout = GuestBootError("guest agent did not answer within 120 s".into());
        assert_eq!(
            custom_boot_error(timeout.clone(), image.path()).0,
            NO_GUEST_AGENT
        );
        std::fs::write(write_unit(image.path()), "[Service]\n").unwrap();
        assert_eq!(
            custom_boot_error(timeout.clone(), image.path()).0,
            timeout.0
        );
    }

    #[test]
    fn a_boot_timeout_with_a_masked_agent_unit_names_the_guest_module() {
        let image = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink("/dev/null", write_unit(image.path())).unwrap();
        let timeout = GuestBootError("guest agent did not answer within 120 s".into());
        assert_eq!(custom_boot_error(timeout, image.path()).0, NO_GUEST_AGENT);
    }
}
