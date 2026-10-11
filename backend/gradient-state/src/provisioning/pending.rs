/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use gradient_types::{ProjectId, RoleId};

#[derive(Debug, Clone)]
pub struct PendingProjectMembership {
    pub project: ProjectId,
    pub role: RoleId,
}

#[derive(Debug, Clone, Default)]
pub struct PendingProjectMemberships(Arc<Mutex<HashMap<String, Vec<PendingProjectMembership>>>>);

impl PendingProjectMemberships {
    pub async fn defer(&self, username: String, membership: PendingProjectMembership) {
        self.entries().entry(username).or_default().push(membership);
    }

    pub async fn of_user(&self, username: &str) -> Vec<PendingProjectMembership> {
        self.entries().get(username).cloned().unwrap_or_default()
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<String, Vec<PendingProjectMembership>>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
