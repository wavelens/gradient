/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use nix::mount::{MsFlags, mount};
use nix::sched::{CloneFlags, unshare};
use nix::sys::statvfs::{FsFlags, statvfs};
use std::path::Path;

/// Like nix-daemon: NixOS binds the store read-only, so remount it writable in a private
/// mount namespace. Must run before any other thread starts, as unshare is per thread.
pub fn make_writable(store: &Path) -> anyhow::Result<()> {
    if !statvfs(store)?.flags().contains(FsFlags::ST_RDONLY) {
        return Ok(());
    }
    unshare(CloneFlags::CLONE_NEWNS)?;
    mount(
        None::<&str>,
        store,
        None::<&str>,
        MsFlags::MS_REMOUNT | MsFlags::MS_BIND,
        None::<&str>,
    )?;
    Ok(())
}
