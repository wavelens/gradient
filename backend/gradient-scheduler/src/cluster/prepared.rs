/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::HashMap;
use std::sync::Arc;

use gradient_util::sync::Mutex;

pub struct PreparedMember {
    pub assignment: crate::jobs::Assignment,
    pub membership: gradient_wire::types::ClusterMembership,
}

#[derive(Clone, Default)]
pub struct PreparedMembers(Arc<Mutex<HashMap<String, PreparedMember>>>);

impl PreparedMembers {
    pub async fn insert(&self, job_id: String, member: PreparedMember) {
        self.0.lock().insert(job_id, member);
    }

    pub async fn take(&self, job_id: &str) -> Option<PreparedMember> {
        self.0.lock().remove(job_id)
    }

    pub async fn forget(&self, job_ids: &[String]) {
        let mut prepared = self.0.lock();
        for job_id in job_ids {
            prepared.remove(job_id);
        }
    }
}
