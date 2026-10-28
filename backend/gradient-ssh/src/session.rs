/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use crate::gateway_user::GatewayTarget;
use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_graph::Graph;
use gradient_scheduler::Scheduler;
use gradient_storage::admission::UploadAdmission;
use gradient_types::permissions::{Permission, PermissionMask, mask_grants};
use gradient_types::*;
use gradient_util::shutdown::CancellationToken;
use std::sync::Arc;

#[derive(Clone)]
pub struct SshHandles {
    pub db: DbContext,
    pub graph: Arc<Graph>,
    pub upload_admission: Arc<UploadAdmission>,
    pub scheduler: Arc<Scheduler>,
}

impl SshHandles {
    pub fn from_state(state: &ServerState, scheduler: Arc<Scheduler>) -> Self {
        Self {
            db: state.db(),
            graph: Arc::clone(&state.graph),
            upload_admission: Arc::clone(&state.memory.upload_admission),
            scheduler,
        }
    }

    #[cfg(test)]
    pub fn for_tests(state: &Arc<ServerState>) -> Self {
        Self::from_state(state, Arc::new(Scheduler::new(Arc::clone(state))))
    }
}

pub struct Session {
    pub handles: SshHandles,
    pub user: MUser,
    pub project: MProject,
    pub permissions: PermissionMask,
    pub caches: Vec<CacheId>,
    pub mapper: StorePathMapper,
    pub closed: CancellationToken,
    pub target: Option<GatewayTarget>,
}

impl Session {
    pub async fn open(
        handles: SshHandles,
        user: MUser,
        project: MProject,
        permissions: PermissionMask,
        caches: Vec<CacheId>,
    ) -> Result<Self, MappingError> {
        let mapper = handles.db.project_mapper(project.id).await?;
        Ok(Self {
            closed: handles.db.shutdown.child_token(),
            handles,
            user,
            project,
            permissions,
            caches,
            mapper,
            target: None,
        })
    }

    pub fn may(&self, permission: Permission) -> bool {
        mask_grants(self.permissions, permission)
    }
}
