/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone, Debug, Default)]
pub struct Draining(Arc<AtomicBool>);

impl Draining {
    pub async fn is_draining(&self) -> bool {
        self.0.load(Ordering::Relaxed)
    }

    pub async fn set(&self, enabled: bool) {
        self.0.store(enabled, Ordering::Relaxed);
    }
}
