/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use super::{LogStorage, NarStore};

#[derive(Clone, Debug)]
pub struct StorageCtx {
    pub nar_storage: NarStore,
    pub log_storage: Arc<dyn LogStorage>,
}
