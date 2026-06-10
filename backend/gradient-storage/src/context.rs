/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use super::{EmailSender, LogStorage, NarStore};

/// Storage-layer slice: the NAR object store, build-log storage, and the email
/// sender. The narrowest context; carried by `DbContext` so `db`
/// and `ci` reach storage without naming the composed `AppState`.
#[derive(Clone, Debug)]
pub struct StorageCtx {
    pub nar_storage: NarStore,
    pub log_storage: Arc<dyn LogStorage>,
    pub email: Arc<dyn EmailSender>,
}
