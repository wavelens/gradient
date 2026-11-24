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
    caches: Vec<CacheId>,
    pub mapper: StorePathMapper,
    pub closed: CancellationToken,
    pub target: Option<GatewayTarget>,
    pub allows_project_credentials_and_privileged_actions: bool,
    pub storage_limit_refusal_reason: tokio::sync::OnceCell<Option<String>>,
}

impl Session {
    pub async fn open(
        handles: SshHandles,
        user: MUser,
        project: MProject,
        permissions: PermissionMask,
        caches: Vec<CacheId>,
        allows_project_credentials_and_privileged_actions: bool,
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
            allows_project_credentials_and_privileged_actions,
            storage_limit_refusal_reason: tokio::sync::OnceCell::new(),
        })
    }

    pub fn with_gateway_target(self, target: Option<GatewayTarget>) -> Self {
        Self { target, ..self }
    }

    pub fn readable_cache_ids_or_closed_error(&self) -> anyhow::Result<&[CacheId]> {
        if self.closed.is_cancelled() {
            anyhow::bail!("SSH session is closed. The cache list is no longer available.");
        }
        Ok(&self.caches)
    }

    pub fn matches_user_project_permissions_and_readable_cache_ids_if_not_closed(
        &self,
        user: UserId,
        project: ProjectId,
        permissions: PermissionMask,
        caches: &[CacheId],
    ) -> bool {
        user == self.user.id
            && project == self.project.id
            && permissions == self.permissions
            && self
                .readable_cache_ids_or_closed_error()
                .is_ok_and(|readable| readable == caches)
    }

    pub fn may(&self, permission: Permission) -> bool {
        mask_grants(self.permissions, permission)
    }
}
