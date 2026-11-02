/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_entity::{IdentityMapping, StorePathMapping};
use gradient_pool::{EveryJobAllowed, WorkerJobPolicy};

use crate::cache_creation_limit::{CacheCreationLimit, NoCacheCreationLimit};
use crate::login_provider::LoginProvider;

#[derive(Clone, Debug)]
pub struct ServerExtensions {
    pub store_path_mapping: Arc<dyn StorePathMapping>,
    pub login_providers: Vec<Arc<dyn LoginProvider>>,
    pub worker_job_policy: Arc<dyn WorkerJobPolicy>,
    pub cache_creation_limit: Arc<dyn CacheCreationLimit>,
}

impl Default for ServerExtensions {
    fn default() -> Self {
        Self {
            store_path_mapping: Arc::new(IdentityMapping),
            login_providers: Vec::new(),
            worker_job_policy: Arc::new(EveryJobAllowed),
            cache_creation_limit: Arc::new(NoCacheCreationLimit),
        }
    }
}
