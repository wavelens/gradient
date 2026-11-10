/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashSet;

use gradient_util::sync::Mutex;

#[derive(Debug, Default)]
pub struct DrainedWorkerSet(Mutex<HashSet<String>>);

impl DrainedWorkerSet {
    pub fn insert(&self, worker_id: &str) {
        self.0.lock().insert(worker_id.to_owned());
    }

    pub fn remove(&self, worker_id: &str) -> bool {
        self.0.lock().remove(worker_id)
    }

    pub fn contains(&self, worker_id: &str) -> bool {
        self.0.lock().contains(worker_id)
    }
}
