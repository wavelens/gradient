/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::path::{Path, PathBuf};

use tokio::sync::watch;
use tracing::warn;

use super::job_channels::GuestChannelInbox;

const MARKER_FILE: &str = "keep-vm-running";

pub struct ExecInbox {
    pub channels: GuestChannelInbox,
    pub keep_vm_running: watch::Receiver<bool>,
}

pub struct KeepVmRunning {
    requested: bool,
    updates: watch::Receiver<bool>,
    marker: Option<PathBuf>,
}

impl KeepVmRunning {
    pub fn new(updates: watch::Receiver<bool>, requested: bool, marker: Option<PathBuf>) -> Self {
        let keep = Self {
            requested,
            updates,
            marker,
        };
        keep.record();
        keep
    }

    pub fn resumed(updates: watch::Receiver<bool>, guest_dir: &Path) -> Self {
        let marker = marker_file(guest_dir);
        let requested = marker.exists();
        Self::new(updates, requested, Some(marker))
    }

    pub fn requested(&self) -> bool {
        self.requested
    }

    pub async fn changed(&mut self) {
        if self.updates.changed().await.is_err() {
            return std::future::pending().await;
        }
        self.requested = *self.updates.borrow_and_update();
        self.record();
    }

    fn record(&self) {
        let Some(marker) = &self.marker else {
            return;
        };
        let written = match self.requested {
            true => std::fs::write(marker, b""),
            false => match std::fs::remove_file(marker) {
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                removed => removed,
            },
        };
        if let Err(e) = written {
            warn!(marker = %marker.display(), error = %e, "cannot record the keep VM running switch");
        }
    }
}

pub fn marker_file(guest_dir: &Path) -> PathBuf {
    guest_dir.join(MARKER_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_switch_survives_a_worker_restart_through_the_guest_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (switch, updates) = watch::channel(false);
        let mut keep = KeepVmRunning::new(updates, true, Some(marker_file(dir.path())));

        let (_, after_restart) = watch::channel(false);
        assert!(KeepVmRunning::resumed(after_restart, dir.path()).requested());

        switch.send_replace(false);
        keep.changed().await;
        assert!(!keep.requested());
        let (_, after_release) = watch::channel(false);
        assert!(!KeepVmRunning::resumed(after_release, dir.path()).requested());
    }
}
