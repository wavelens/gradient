/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_core::ServerState;
use gradient_db::permissions::{Permission, PermissionMask, mask_grants};
use gradient_types::*;
use std::sync::Arc;

pub struct Session {
    pub state: Arc<ServerState>,
    pub user: MUser,
    pub project: MProject,
    pub permissions: PermissionMask,
    pub caches: Vec<CacheId>,
}

impl Session {
    pub fn may(&self, permission: Permission) -> bool {
        mask_grants(self.permissions, permission)
    }
}
