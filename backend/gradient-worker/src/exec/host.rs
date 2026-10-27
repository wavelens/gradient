/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gradient_vm::{DeviceInventory, GuestPool};

use crate::config::ExecArgs;
use crate::executor::AbortSignal;

pub struct ExecHost {
    pub pool: Arc<GuestPool>,
    pub devices: DeviceInventory,
    pub config: ExecArgs,
    pub worker_id: String,
    pub zone: Option<String>,
    stopping_with_guests_kept: AtomicBool,
}

impl ExecHost {
    pub fn keep_guests_through_the_stop(&self) {
        self.stopping_with_guests_kept.store(true, Ordering::SeqCst);
    }

    pub fn leaves_guest_running(&self, abort: &AbortSignal) -> bool {
        !abort.was_requested() && self.stopping_with_guests_kept.load(Ordering::SeqCst)
    }
}
