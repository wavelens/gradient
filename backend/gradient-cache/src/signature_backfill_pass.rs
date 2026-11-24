/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_db::store_path_content_variant_trust::{
    BackfillRowsWrittenOrSkippedForLockHeldElsewhere,
    read_subscriptions_owing_signature_backfill_after_cursor,
    write_unsigned_signature_placeholders_for_project_earlier_outputs_then_clear_owed_subscription_requested_time_in_locked_transaction_and_commit,
};
use gradient_db::{DbContext, WorkerDb};
use gradient_util::supervision::{Pass, PassError};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Notify;
use tracing::warn;

const OWED_SUBSCRIPTIONS_READ_PER_BACKFILL_PAGE: u64 = 100;

pub(crate) struct StartedSigningSubscriptionSignatureBackfillPass {
    pub(crate) db: WorkerDb,
    pub(crate) period: Duration,
    pub(crate) wake: Arc<Notify>,
    pub(crate) signature_sweep_publish_round_database_context: DbContext,
}

impl Pass for StartedSigningSubscriptionSignatureBackfillPass {
    const NAME: &'static str = "signature-backfill";
    const BUDGET: Duration = Duration::from_secs(3600);
    const RUN_AT_START: bool = true;

    fn period(&self) -> Duration {
        self.period
    }

    fn wake(&self) -> Option<Arc<Notify>> {
        Some(Arc::clone(&self.wake))
    }

    async fn run(&self) -> Result<(), PassError> {
        let mut after = None;
        loop {
            let page = read_subscriptions_owing_signature_backfill_after_cursor(
                &self.db,
                after.as_ref(),
                OWED_SUBSCRIPTIONS_READ_PER_BACKFILL_PAGE,
            )
            .await?;
            for owed in &page {
                match write_unsigned_signature_placeholders_for_project_earlier_outputs_then_clear_owed_subscription_requested_time_in_locked_transaction_and_commit(
                    &self.db,
                    owed,
                )
                .await
                {
                    Ok(BackfillRowsWrittenOrSkippedForLockHeldElsewhere::BackfillWrotePlaceholderRows(written)) if written > 0 => {
                        self.signature_sweep_publish_round_database_context
                            .queue_request_signature_sweep_to_check_all_parent_builds();
                    }
                    Ok(_) => {}
                    Err(error) => warn!(
                        subscription = %owed.id,
                        project = %owed.project,
                        cache = %owed.cache,
                        %error,
                        "signature backfill for the cache subscription failed. Still owed, retry next period."
                    ),
                }
            }
            if (page.len() as u64) < OWED_SUBSCRIPTIONS_READ_PER_BACKFILL_PAGE {
                return Ok(());
            }
            after = page.into_iter().last();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::test_server_state;
    use gradient_fixtures::shape::shape;
    use gradient_storage::NarStore;
    use gradient_types::*;

    #[tokio::test]
    async fn run_with_owed_subscription_writes_placeholders_clears_requested_time_and_wakes_sign_sweep()
     {
        let (output, cache) = ("99999999999999999999999999999999", CacheId::now_v7());
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&format!("/nix/store/{}-output.drv", "r".repeat(32)))
            .output(output)
            .job()
            .cached_path(output, "output")
            .into_cache(cache)
            .keyed("signing-key")
            .into_database()
            .await;
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = test_server_state(
            NarStore::local(tmp.path()).expect("store"),
            db.connection(),
            |_| {},
        );
        let pass = StartedSigningSubscriptionSignatureBackfillPass {
            db: state.worker_db.clone(),
            period: Duration::from_secs(state.config.cache.sign_sweep_interval_secs.get()),
            wake: Arc::clone(&state.wake.run_signature_backfill_now),
            signature_sweep_publish_round_database_context: state.db(),
        };

        pass.run().await.expect("the pass runs");

        let sign_sweep_woken = tokio::time::timeout(
            Duration::from_secs(1),
            state.wake.run_signature_sweep_now.notified(),
        )
        .await
        .is_ok();
        assert_eq!(
            (
                db.texts(&format!(
                    "SELECT id::text FROM cached_path_signature WHERE cache = '{cache}'"
                ))
                .await
                .len(),
                db.texts(
                    "SELECT coalesce(unsigned_signature_backfill_requested_at::text, 'NULL') \
                     FROM project_cache"
                )
                .await,
                sign_sweep_woken,
            ),
            (1, vec!["NULL".to_owned()], true),
            "A single run of this pass on the server right after startup can write the placeholder rows for project output. That run can clear the request time and start the sign sweep."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn cache_child_specs_hold_signature_backfill_pass_with_wake_and_run_at_start() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = test_server_state(
            NarStore::local(tmp.path()).expect("store"),
            gradient_fixtures::mock::empty_db(),
            |_| {},
        );
        let pass = StartedSigningSubscriptionSignatureBackfillPass {
            db: state.worker_db.clone(),
            period: Duration::from_secs(state.config.cache.sign_sweep_interval_secs.get()),
            wake: Arc::clone(&state.wake.run_signature_backfill_now),
            signature_sweep_publish_round_database_context: state.db(),
        };
        assert_eq!(
            (
                crate::schedule::child_specs(&state).iter().any(
                    |spec| spec.name() == StartedSigningSubscriptionSignatureBackfillPass::NAME
                ),
                pass.wake()
                    .is_some_and(|wake| Arc::ptr_eq(&wake, &state.wake.run_signature_backfill_now)),
                StartedSigningSubscriptionSignatureBackfillPass::RUN_AT_START,
            ),
            (true, true, true),
            "A signature backfill pass is part of the cache path. This pass can run at startup and wait for its own wake."
        );
    }
}
