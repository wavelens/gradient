/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::store_path_content_variant_trust::insert_unsigned_signature_placeholders_for_queued_pushed_trusted_content_variants_by_project;
use gradient_db::{AlreadyStoredPushCacheQueryStorePathHashesByProject, CacheDb};
use gradient_util::supervision::{Pass, PassError};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;

pub(crate) struct PushedStorePathSignaturePlaceholderInsertPass {
    pub(crate) db: CacheDb,
    pub(crate) queued: AlreadyStoredPushCacheQueryStorePathHashesByProject,
    pub(crate) period: Duration,
    pub(crate) wake: Arc<Notify>,
}

impl Pass for PushedStorePathSignaturePlaceholderInsertPass {
    const NAME: &'static str = "pushed-store-path-unsigned-signature-placeholder-insert";
    const BUDGET: Duration = Duration::from_secs(1800);

    fn period(&self) -> Duration {
        self.period
    }

    fn wake(&self) -> Option<Arc<Notify>> {
        Some(Arc::clone(&self.wake))
    }

    async fn run(&self) -> Result<(), PassError> {
        insert_unsigned_signature_placeholders_for_queued_pushed_trusted_content_variants_by_project(
            &self.db,
            &self.queued,
        )
        .await;
        Ok(())
    }
}
