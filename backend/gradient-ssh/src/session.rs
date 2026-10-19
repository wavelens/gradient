/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ServerState;
use gradient_db::DbContext;
use gradient_graph::Graph;
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
}

impl SshHandles {
    pub fn from_state(state: &ServerState) -> Self {
        Self {
            db: state.db(),
            graph: Arc::clone(&state.graph),
            upload_admission: Arc::clone(&state.memory.upload_admission),
        }
    }
}

pub struct Session {
    pub handles: SshHandles,
    pub user: MUser,
    pub project: MProject,
    pub permissions: PermissionMask,
    pub caches: Vec<CacheId>,
    pub closed: CancellationToken,
}

impl Session {
    pub fn may(&self, permission: Permission) -> bool {
        mask_grants(self.permissions, permission)
    }

    pub fn store_path_mapper(&self) -> StorePathMapper {
        self.handles
            .db
            .store_path_mapper(MappingContext::Project(self.project.id))
    }
}
