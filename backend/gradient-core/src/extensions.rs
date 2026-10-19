/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use gradient_entity::{IdentityMapping, StorePathMapping};

#[derive(Clone, Debug)]
pub struct ServerExtensions {
    pub store_path_mapping: Arc<dyn StorePathMapping>,
}

impl Default for ServerExtensions {
    fn default() -> Self {
        Self {
            store_path_mapping: Arc::new(IdentityMapping),
        }
    }
}
