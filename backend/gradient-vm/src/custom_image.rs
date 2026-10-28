/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::{Path, PathBuf};

use crate::agent::GuestBootError;
use crate::hypervisor::GuestImage;

pub const NO_GUEST_AGENT: &str =
    "guest image has no gradient agent, import the gradient.guest module";
const GUEST_AGENT_UNIT: &str = "etc/systemd/system/gradient-guest-agent.service";

pub fn image_from_toplevel(
    toplevel: &Path,
    sized_like: &GuestImage,
) -> Result<GuestImage, GuestBootError> {
    let kernel = system_file(toplevel, "kernel")?;
    let initrd = system_file(toplevel, "initrd")?;
    let kernel_params = std::fs::read_to_string(system_file(toplevel, "kernel-params")?)
        .map_err(|e| not_bootable(toplevel, &format!("kernel-params unreadable: {e}")))?;
    Ok(GuestImage {
        kernel,
        initrd,
        cmdline: format!("init={}/init {}", toplevel.display(), kernel_params.trim()),
        cpus: sized_like.cpus,
        memory_mb: sized_like.memory_mb,
    })
}

fn has_guest_agent(toplevel: &Path) -> bool {
    toplevel.join(GUEST_AGENT_UNIT).exists()
}

pub fn custom_boot_error(error: GuestBootError, toplevel: &Path) -> GuestBootError {
    if has_guest_agent(toplevel) {
        error
    } else {
        GuestBootError(NO_GUEST_AGENT.to_owned())
    }
}

fn system_file(toplevel: &Path, name: &str) -> Result<PathBuf, GuestBootError> {
    std::fs::canonicalize(toplevel.join(name))
        .map_err(|_| not_bootable(toplevel, &format!("no {name}")))
}

fn not_bootable(toplevel: &Path, reason: &str) -> GuestBootError {
    GuestBootError(format!(
        "image {} is not a NixOS system: {reason}",
        toplevel.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn worker_image(cpus: u8, memory_mb: u64) -> GuestImage {
        GuestImage {
            kernel: "/nix/store/x-guest/kernel".into(),
            initrd: "/nix/store/x-guest/initrd".into(),
            cmdline: "console=ttyS0".into(),
            cpus,
            memory_mb,
        }
    }

    fn toplevel() -> tempfile::TempDir {
        let top = tempfile::tempdir().unwrap();
        for name in ["kernel", "initrd", "init"] {
            std::fs::write(top.path().join(name), name).unwrap();
        }
        std::fs::write(
            top.path().join("kernel-params"),
            "console=ttyS0 loglevel=4\n",
        )
        .unwrap();
        top
    }

    fn write_unit(top: &Path) {
        let unit = top.join(GUEST_AGENT_UNIT);
        std::fs::create_dir_all(unit.parent().unwrap()).unwrap();
        std::fs::write(unit, "[Service]\n").unwrap();
    }

    #[test]
    fn a_toplevel_boots_its_own_kernel_with_its_kernel_params() {
        let top = toplevel();
        let image = image_from_toplevel(top.path(), &worker_image(2, 2048)).unwrap();
        assert_eq!(
            image.cmdline,
            format!(
                "init={}/init console=ttyS0 loglevel=4",
                top.path().display()
            )
        );
        assert_eq!(
            (image.kernel, image.initrd),
            (
                top.path().join("kernel").canonicalize().unwrap(),
                top.path().join("initrd").canonicalize().unwrap()
            )
        );
        assert_eq!((image.cpus, image.memory_mb), (2, 2048));
    }

    #[test]
    fn a_toplevel_without_a_kernel_is_not_bootable() {
        let empty = tempfile::tempdir().unwrap();
        let err = image_from_toplevel(empty.path(), &worker_image(2, 2048)).unwrap_err();
        assert!(
            err.0.contains(&empty.path().display().to_string())
                && err.0.contains("not a NixOS system"),
            "{}",
            err.0
        );
    }

    #[test]
    fn a_boot_timeout_without_the_agent_unit_names_the_guest_module() {
        let top = toplevel();
        let timeout = GuestBootError("guest agent did not answer within 120 s".into());
        assert_eq!(
            custom_boot_error(timeout.clone(), top.path()).0,
            NO_GUEST_AGENT
        );
        write_unit(top.path());
        assert_eq!(custom_boot_error(timeout.clone(), top.path()).0, timeout.0);
    }
}
