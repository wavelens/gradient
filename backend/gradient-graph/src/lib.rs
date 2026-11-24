/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

#![recursion_limit = "256"]

pub mod messages;
pub mod writer;

#[cfg(test)]
mod cases;
mod claims;
mod continuation;
mod demote;
pub mod derivation_closure_input_addressed_output_verification;
mod exec;
mod gc;
mod known;
mod nar;
pub mod policy;
pub mod read_derivation_from_trusted_nar;
mod record;
mod requeue;
mod self_heal;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod transition;

use std::sync::{Arc, OnceLock};
use std::time::Instant;

use anyhow::Context as _;
use gradient_db::DbContext;
use gradient_db::graph::consistency::{
    ConsistencyCheckRepairScopeChunkProgress, ConsistencyReport, run_consistency_check_steps,
};
use gradient_types::DerivationId;
use gradient_types::events::{Event, EventBus, graph};
use gradient_util::supervision::{ChildCtx, ChildSpec, SupervisorHealth};
use gradient_util::sync::Mutex;
use ractor::rpc::CallResult;
use ractor::{ACTIVE_STATES, Actor, ActorCell, ActorRef, RpcReplyPort, SpawnErr};
use tokio::sync::watch;

use claims::PublishFailurePageSizeAndWaitingParentBuilds;
pub use claims::{
    DEPENDENCY_OUTPUT_SIGNING_SELECT_STATEMENT_TIMEOUT, DependencyOutputPublishRoundResult,
    PARENT_BUILD_PAGE_READ_STATEMENT_TIMEOUT,
    PARENT_BUILD_SKIP_DURATION_AFTER_ONLY_PARENT_PAGE_PUBLISH_FAILURE,
    SignatureRowForGraphWriterInsert, UnfinishedRequestsError,
};
pub use messages::*;
pub use policy::retry_backoff_elapsed;
use writer::{CALL_TIMEOUT, GraphArgs, GraphMsg, GraphWriter, HEALTH_NAME};

#[derive(Debug)]
pub struct ConsistencyCheckDemotionFailed {
    pub consistency_report_after_failed_demotion: Option<ConsistencyReport>,
    demotion_error: anyhow::Error,
}

impl std::fmt::Display for ConsistencyCheckDemotionFailed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Failed to demote a completed build with no stored copy for a required output.")
    }
}

impl std::error::Error for ConsistencyCheckDemotionFailed {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.demotion_error.as_ref())
    }
}

pub struct Graph {
    writer: watch::Sender<Option<ActorRef<GraphMsg>>>,
    events: OnceLock<EventBus>,
    ctx: OnceLock<DbContext>,
    publish_failure_page_size_and_waiting_parent_builds:
        Mutex<PublishFailurePageSizeAndWaitingParentBuilds>,
    #[cfg(feature = "stub")]
    stub: bool,
}

struct FailedPublishRoundPage {
    page: Vec<DerivationId>,
    published_parent_builds: Vec<DerivationId>,
    error: anyhow::Error,
}

enum PublishRoundChoiceAfterFailedPage {
    SetAsideParentBuildAndMovePastPage(Vec<DerivationId>),
    RetryFromSameCursorWithHalfPageSize,
    StopPublishRoundWithError(anyhow::Error),
}

const MAX_PARENT_BUILDS_LISTED_IN_FAILED_PAGE_WARNING: usize = 20;

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph").finish_non_exhaustive()
    }
}

impl Graph {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            writer: watch::channel(None).0,
            events: OnceLock::new(),
            ctx: OnceLock::new(),
            publish_failure_page_size_and_waiting_parent_builds: Default::default(),
            #[cfg(feature = "stub")]
            stub: false,
        })
    }

    #[cfg(feature = "stub")]
    pub fn stub() -> Arc<Self> {
        Arc::new(Self {
            writer: watch::channel(None).0,
            events: OnceLock::new(),
            ctx: OnceLock::new(),
            publish_failure_page_size_and_waiting_parent_builds: Default::default(),
            stub: true,
        })
    }

    pub fn child_spec(self: &Arc<Self>, ctx: DbContext) -> ChildSpec {
        let graph = Arc::clone(self);
        ChildSpec::Custom {
            name: HEALTH_NAME,
            stop_last: true,
            spawn: Arc::new(move |child: ChildCtx| {
                let graph = Arc::clone(&graph);
                let ctx = ctx.clone();
                Box::pin(async move {
                    let writer = graph
                        .spawn(ctx, Some(child.health), Some(child.parent))
                        .await?;
                    Ok(writer.get_cell())
                })
            }),
        }
    }

    pub async fn spawn(
        &self,
        ctx: DbContext,
        health: Option<Arc<SupervisorHealth>>,
        parent: Option<ActorCell>,
    ) -> Result<ActorRef<GraphMsg>, SpawnErr> {
        let _ = self.events.set(ctx.events.clone());
        let _ = self.ctx.set(ctx.clone());
        let args = GraphArgs { ctx, health };
        let (writer, _) = match parent {
            Some(parent) => Actor::spawn_linked(None, GraphWriter, args, parent).await?,
            None => Actor::spawn(None, GraphWriter, args).await?,
        };
        self.writer.send_replace(Some(writer.clone()));
        Ok(writer)
    }

    fn announce<E: Into<Event>>(&self, event: impl FnOnce() -> E) {
        if let Some(bus) = self.events.get() {
            bus.publish(event());
        }
    }

    async fn live(&self) -> anyhow::Result<ActorRef<GraphMsg>> {
        let mut rx = self.writer.subscribe();
        let live = tokio::time::timeout(
            CALL_TIMEOUT,
            rx.wait_for(|a| {
                a.as_ref()
                    .is_some_and(|a| ACTIVE_STATES.contains(&a.get_status()))
            }),
        )
        .await
        .map_err(|_| anyhow::anyhow!("graph writer unavailable"))?
        .map_err(|_| anyhow::anyhow!("graph writer closed"))?;
        Ok(live.clone().expect("wait_for guarantees Some"))
    }

    async fn call<T: Send + 'static>(
        &self,
        msg: impl FnOnce(RpcReplyPort<anyhow::Result<T>>) -> GraphMsg,
    ) -> anyhow::Result<T> {
        match self.live().await?.call(msg, None).await {
            Ok(CallResult::Success(result)) => result,
            Ok(CallResult::SenderError | CallResult::Timeout) => {
                Err(anyhow::anyhow!("graph writer dropped the reply"))
            }
            Err(e) => Err(anyhow::anyhow!("graph writer unreachable: {e}")),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(eval_id = %batch.evaluation, derivations = batch.derivations.len()))]
    pub async fn record(&self, batch: RecordBatch) -> anyhow::Result<RecordReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(RecordReport::default());
        }
        let report = self.call(|reply| GraphMsg::Record(batch, reply)).await?;
        self.announce(|| graph::Recorded {
            evaluation_id: report.evaluation,
            task: report.task,
            walked: report.walked,
            entry_points: report.entry_points.len(),
            skipped: report.skipped,
        });
        Ok(report)
    }

    /// The read is going to the pool, not behind the graph writer.
    /// A committed subtree can only ever gain its record.
    /// A read that misses a queued write is pruning less, never wrongly.
    #[tracing::instrument(level = "debug", skip_all, fields(paths = drv_hashes.len()))]
    pub async fn known_derivations(&self, drv_hashes: Vec<String>) -> anyhow::Result<Vec<String>> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(Vec::new());
        }
        let ctx = self
            .ctx
            .get()
            .ok_or_else(|| anyhow::anyhow!("graph writer never started"))?;
        Ok(known::prunable(&ctx.worker_db, drv_hashes).await?)
    }

    pub async fn upstream_hits(
        &self,
        hits: std::collections::HashMap<String, UpstreamHit>,
    ) -> anyhow::Result<()> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(());
        }
        self.call(|reply| GraphMsg::UpstreamHits(hits, reply)).await
    }

    pub async fn upstream_probed(&self, shared_builds: Vec<DerivationId>) -> anyhow::Result<()> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(());
        }
        self.call(|reply| GraphMsg::UpstreamProbed(shared_builds, reply))
            .await
    }

    pub async fn commit_nar(&self, commit: NarCommit) -> anyhow::Result<NarCommitted> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(NarCommitted {
                cached_path: gradient_types::ids::CachedPathId::now_v7(),
                nar_object_storage_key: String::new(),
                created: true,
                signed: Vec::new(),
            });
        }
        let committed = self
            .call(|reply| GraphMsg::CommitNar(commit, reply))
            .await?;
        self.announce(|| graph::NarCommitted {
            cached_path: committed.cached_path,
            created: committed.created,
        });
        Ok(committed)
    }

    pub async fn transition(&self, transition: Transition) -> anyhow::Result<TransitionReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(TransitionReport::default());
        }
        let report = self
            .call(|reply| GraphMsg::Transition(transition, reply))
            .await?;
        self.announce(|| graph::Transitioned {
            aborted: report.aborted_shared_builds.len(),
            prioritized: report.prioritized_shared_builds.len(),
        });
        Ok(report)
    }

    /// Page reads and signatures happen outside the graph writer. Only inserts of a non-empty page
    /// go out as exactly one writer message. Rounds stopped by the deadline always keep the cursor
    /// and always request one more signature sweep wake.
    pub async fn run_publish_round_for_signed_dependency_outputs_of_parent_builds_in_recent_evaluations_until_deadline(
        &self,
        start: Option<DerivationId>,
        maximum_parent_builds_per_page: usize,
        deadline: Instant,
    ) -> DependencyOutputPublishRoundResult {
        let mut round = DependencyOutputPublishRoundResult {
            last_handled_parent_build_id: start,
            ..Default::default()
        };
        #[cfg(feature = "stub")]
        if self.stub {
            return round;
        }
        let Some(ctx) = self.ctx.get() else {
            round.failure = Some(anyhow::anyhow!("graph writer never started"));
            return round;
        };
        let mut segments = vec![(
            start.unwrap_or_else(DerivationId::nil),
            DerivationId::new(uuid::Uuid::max()),
        )];
        segments.extend(start.map(|start| (DerivationId::nil(), start)));
        let segment_count = segments.len();
        for (segment_index, (mut after, upto)) in segments.into_iter().enumerate() {
            loop {
                let parent_builds_per_page = self
                    .publish_failure_page_size_and_waiting_parent_builds
                    .lock()
                    .next_page_size_after_failures(maximum_parent_builds_per_page);
                let page = match self
                    .read_parent_build_page_sign_dependency_outputs_and_send_insert_to_graph_writer(
                        ctx,
                        after,
                        upto,
                        parent_builds_per_page,
                        &mut round,
                    )
                    .await
                {
                    Ok(page) => {
                        self.publish_failure_page_size_and_waiting_parent_builds
                            .lock()
                            .parent_builds_per_page = Some(
                            parent_builds_per_page
                                .saturating_mul(2)
                                .min(maximum_parent_builds_per_page),
                        );
                        page
                    }
                    Err(failed_page) => {
                        round.failed_page_count += 1;
                        match self.log_failed_publish_page_and_decide_round_next_step(
                            failed_page,
                            parent_builds_per_page,
                            &mut round,
                        ) {
                            PublishRoundChoiceAfterFailedPage::SetAsideParentBuildAndMovePastPage(
                                page,
                            ) => page,
                            PublishRoundChoiceAfterFailedPage::RetryFromSameCursorWithHalfPageSize => {
                                if Instant::now() >= deadline {
                                    round.stopped_at_deadline_with_parent_builds_remaining = true;
                                    ctx.queue_request_signature_sweep_to_check_all_parent_builds();
                                    return round;
                                }
                                continue;
                            }
                            PublishRoundChoiceAfterFailedPage::StopPublishRoundWithError(error) => {
                                round.failure = Some(error);
                                return round;
                            }
                        }
                    }
                };
                let Some(&page_last) = page.last() else {
                    break;
                };
                round.last_handled_parent_build_id = Some(page_last);
                after = page_last;
                let page_was_full = page.len() >= parent_builds_per_page;
                let parent_builds_may_be_left = page_was_full || segment_index + 1 < segment_count;
                if parent_builds_may_be_left && Instant::now() >= deadline {
                    round.stopped_at_deadline_with_parent_builds_remaining = true;
                    ctx.queue_request_signature_sweep_to_check_all_parent_builds();
                    return round;
                }
                if !page_was_full {
                    break;
                }
            }
        }
        round
    }

    pub async fn sign_and_publish_dependency_outputs_for_requested_parent_builds_and_parent_builds_of_requested_dependencies_until_read_failure(
        &self,
        request: gradient_db::DependencyOutputSigningRequest,
        maximum_parent_builds_per_page: usize,
    ) -> Result<DependencyOutputPublishRoundResult, UnfinishedRequestsError> {
        let mut round = DependencyOutputPublishRoundResult::default();
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(round);
        }
        let unfinished =
            |error, parent_builds: &[DerivationId]| UnfinishedRequestsError {
                error,
                unfinished: gradient_db::DependencyOutputSigningRequest {
                    parent_build_derivation_ids_to_check_for_dependency_outputs_to_sign:
                        parent_builds.iter().copied().collect(),
                    ..Default::default()
                },
            };
        let Some(ctx) = self.ctx.get() else {
            return Err(UnfinishedRequestsError {
                error: anyhow::anyhow!("graph writer never started"),
                unfinished: request,
            });
        };
        let requested_parent_builds: Vec<DerivationId> = request
            .parent_build_derivation_ids_to_check_for_dependency_outputs_to_sign
            .iter()
            .copied()
            .collect();
        let requested_dependencies: Vec<DerivationId> = request
            .dependency_derivation_ids_with_new_outputs
            .iter()
            .copied()
            .collect();
        let parent_builds = match claims::parent_builds_with_dependency_outputs_to_publish(
            &ctx.worker_db,
            &requested_parent_builds,
            &requested_dependencies,
        )
        .await
        {
            Ok(parent_builds) => parent_builds,
            Err(error) => {
                return Err(UnfinishedRequestsError {
                    error,
                    unfinished: request,
                });
            }
        };
        let mut remaining = &parent_builds[..];
        while !remaining.is_empty() {
            let parent_builds_per_page = self
                .publish_failure_page_size_and_waiting_parent_builds
                .lock()
                .next_page_size_after_failures(maximum_parent_builds_per_page);
            let (page, rest) = remaining.split_at(parent_builds_per_page.min(remaining.len()));
            match self
                .sign_parent_build_page_dependency_outputs_and_send_insert_to_graph_writer(
                    ctx,
                    page.to_vec(),
                    &mut round,
                )
                .await
            {
                Ok(_) => {
                    self.publish_failure_page_size_and_waiting_parent_builds
                        .lock()
                        .parent_builds_per_page = Some(
                        parent_builds_per_page
                            .saturating_mul(2)
                            .min(maximum_parent_builds_per_page),
                    );
                    remaining = rest;
                }
                Err(failed_page) => {
                    round.failed_page_count += 1;
                    match self.log_failed_publish_page_and_decide_round_next_step(
                        failed_page,
                        parent_builds_per_page,
                        &mut round,
                    ) {
                        PublishRoundChoiceAfterFailedPage::SetAsideParentBuildAndMovePastPage(
                            _,
                        ) => {
                            remaining = rest;
                        }
                        PublishRoundChoiceAfterFailedPage::RetryFromSameCursorWithHalfPageSize => {}
                        PublishRoundChoiceAfterFailedPage::StopPublishRoundWithError(error) => {
                            return Err(unfinished(error, remaining));
                        }
                    }
                }
            }
        }
        Ok(round)
    }

    async fn read_parent_build_page_sign_dependency_outputs_and_send_insert_to_graph_writer(
        &self,
        ctx: &DbContext,
        after: DerivationId,
        upto: DerivationId,
        parent_builds_per_page: usize,
        round: &mut DependencyOutputPublishRoundResult,
    ) -> Result<Vec<DerivationId>, FailedPublishRoundPage> {
        let page = claims::read_page_of_parent_builds_with_dependency_outputs_to_publish(
            &ctx.worker_db,
            after,
            upto,
            parent_builds_per_page,
        )
        .await
        .map_err(|error| FailedPublishRoundPage {
            page: Vec::new(),
            published_parent_builds: Vec::new(),
            error,
        })?;
        self.sign_parent_build_page_dependency_outputs_and_send_insert_to_graph_writer(
            ctx, page, round,
        )
        .await
    }

    async fn sign_parent_build_page_dependency_outputs_and_send_insert_to_graph_writer(
        &self,
        ctx: &DbContext,
        page: Vec<DerivationId>,
        round: &mut DependencyOutputPublishRoundResult,
    ) -> Result<Vec<DerivationId>, FailedPublishRoundPage> {
        let published_parent_builds = self
            .publish_failure_page_size_and_waiting_parent_builds
            .lock()
            .drop_expired_set_asides_and_return_page_parent_builds_not_set_aside(
                &page,
                Instant::now(),
            );
        if published_parent_builds.is_empty() {
            return Ok(page);
        }
        let publish = async {
            let signed_dependency_output_rows_to_insert =
                claims::select_and_sign_dependency_outputs_of_parent_build_page(
                    ctx,
                    &published_parent_builds,
                )
                .await?;
            if !signed_dependency_output_rows_to_insert.is_empty() {
                self.transition(Transition::SignStoredInputsIntoReadWriteBinaryCache {
                    signed_dependency_output_rows_to_insert,
                })
                .await?;
                round.insert_messages_sent_to_graph_writer += 1;
            }
            anyhow::Ok(())
        };
        match publish.await {
            Ok(()) => Ok(page),
            Err(error) => Err(FailedPublishRoundPage {
                page,
                published_parent_builds,
                error,
            }),
        }
    }

    fn log_failed_publish_page_and_decide_round_next_step(
        &self,
        failed_page: FailedPublishRoundPage,
        parent_builds_per_page: usize,
        round: &mut DependencyOutputPublishRoundResult,
    ) -> PublishRoundChoiceAfterFailedPage {
        let FailedPublishRoundPage {
            page,
            published_parent_builds,
            error,
        } = failed_page;
        tracing::debug!(
            parent_build_count = page.len(),
            error = format!("{error:#}"),
            "publish of dependency outputs: full error of the failed page"
        );
        let (error_kind, sqlstate) =
            gradient_db::database_error_kind_text_and_sqlstate_for_warning_log(&error);
        let mut failure_state = self
            .publish_failure_page_size_and_waiting_parent_builds
            .lock();
        if let [failing_parent_build] = published_parent_builds[..] {
            failure_state.set_aside_end_by_parent_build.insert(
                failing_parent_build,
                Instant::now() + PARENT_BUILD_SKIP_DURATION_AFTER_ONLY_PARENT_PAGE_PUBLISH_FAILURE,
            );
            round
                .parent_build_ids_set_aside_in_publish_round
                .push(failing_parent_build);
            tracing::warn!(
                parent_build = ?failing_parent_build,
                set_aside_seconds = PARENT_BUILD_SKIP_DURATION_AFTER_ONLY_PARENT_PAGE_PUBLISH_FAILURE.as_secs(),
                error_kind,
                sqlstate = ?sqlstate,
                "publish of dependency outputs: failure in a page of one parent build. Set this parent build aside for a waiting period. Skip it and move on."
            );
            return PublishRoundChoiceAfterFailedPage::SetAsideParentBuildAndMovePastPage(page);
        }
        if page.is_empty() && parent_builds_per_page <= 1 {
            return PublishRoundChoiceAfterFailedPage::StopPublishRoundWithError(error);
        }
        let failed_page_size = if page.is_empty() {
            parent_builds_per_page
        } else {
            page.len()
        };
        let smaller_parent_builds_per_page = (failed_page_size / 2).max(1);
        failure_state.parent_builds_per_page = Some(smaller_parent_builds_per_page);
        tracing::warn!(
            parent_builds = ?&page[..page.len().min(MAX_PARENT_BUILDS_LISTED_IN_FAILED_PAGE_WARNING)],
            parent_build_count = page.len(),
            parent_builds_per_page = smaller_parent_builds_per_page,
            error_kind,
            sqlstate = ?sqlstate,
            "publish of dependency outputs: page failed, retrying from the same cursor with a smaller page"
        );
        PublishRoundChoiceAfterFailedPage::RetryFromSameCursorWithHalfPageSize
    }

    pub async fn run_consistency_checks(&self) -> anyhow::Result<ConsistencyReport> {
        self.run_consistency_check_steps_from_progress(
            ConsistencyCheckRepairScopeChunkProgress::default(),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn run_consistency_checks_scanning_open_builds_per_transaction(
        &self,
        open_builds_scanned_per_transaction: usize,
    ) -> anyhow::Result<ConsistencyReport> {
        self.run_consistency_check_steps_from_progress(
            ConsistencyCheckRepairScopeChunkProgress::new_with_open_builds_per_transaction(
                open_builds_scanned_per_transaction,
            ),
        )
        .await
    }

    async fn run_consistency_check_steps_from_progress(
        &self,
        starting_progress: ConsistencyCheckRepairScopeChunkProgress,
    ) -> anyhow::Result<ConsistencyReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(ConsistencyReport::default());
        }
        let is_postgres_lock_timeout_error = |e: &anyhow::Error| {
            writer::postgres_sqlstate_from_error_chain(e).as_deref() == Some("55P03")
        };
        let demotion_error_to_return_after_consistency_check_steps = match self
            .call(|reply| {
                GraphMsg::Transition(
                    Transition::DemoteCompletedBuildsMissingNeededStoredOutputCopies,
                    reply,
                )
            })
            .await
        {
            Ok(_) => None,
            Err(e) if is_postgres_lock_timeout_error(&e) => {
                tracing::warn!(
                    step = writer::DEMOTE_COMPLETED_BUILDS_MISSING_NEEDED_STORED_OUTPUT_COPIES_STEP_NAME,
                    error = %format_args!("{e:#}"),
                    "consistency check step hit a lock timeout. skipping to the next step"
                );
                None
            }
            Err(e) => Some(e),
        };
        let consistency_check_steps_result = run_consistency_check_steps(
            starting_progress,
            is_postgres_lock_timeout_error,
            move |step, progress| async move {
                let message = Transition::RunConsistencyCheckStep { step, progress };
                self.call(|reply| GraphMsg::Transition(message, reply))
                    .await
                    .and_then(|report| {
                        report.consistency_check_progress.context(
                            "The graph writer did not return the progress of the consistency check",
                        )
                    })
                    .with_context(|| {
                        format!("Step {} of the consistency check failed", step.step_name())
                    })
            },
        )
        .await;
        let Some(demotion_error) = demotion_error_to_return_after_consistency_check_steps else {
            return consistency_check_steps_result;
        };
        let consistency_report_after_failed_demotion = consistency_check_steps_result
            .inspect_err(|e| {
                tracing::error!(
                    error = %format_args!("{e:#}"),
                    "The demotion failed. The integrity check failed as well."
                );
            })
            .ok();
        Err(ConsistencyCheckDemotionFailed {
            consistency_report_after_failed_demotion,
            demotion_error,
        }
        .into())
    }

    pub async fn requeue(&self, scope: RequeueScope) -> anyhow::Result<u64> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(0);
        }
        let requeued = self.call(|reply| GraphMsg::Requeue(scope, reply)).await?;
        self.announce(|| graph::Requeued { requeued });
        Ok(requeued)
    }

    pub async fn demote(&self, demotion: Demotion) -> anyhow::Result<DemoteReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(DemoteReport::default());
        }
        let report = self.call(|reply| GraphMsg::Demote(demotion, reply)).await?;
        self.announce(|| graph::Demoted {
            producers: report.producers.len(),
            others_remain: report.others_remain,
        });
        Ok(report)
    }

    pub async fn gc(&self, request: GcRequest) -> anyhow::Result<GcReport> {
        #[cfg(feature = "stub")]
        if self.stub {
            return Ok(GcReport::default());
        }
        let report = self.call(|reply| GraphMsg::Gc(request, reply)).await?;
        self.announce(|| graph::Collected {
            derivations: report.deleted_derivations.len(),
            evaluations: report.deleted_evaluations.len(),
            retired_paths: report.unreferenced_cached_path_object_storage_keys.len(),
        });
        Ok(report)
    }
}

/// The linker is dropping an rlib nothing mentions, including its `gradient_db::sql!` entries.
/// This call is keeping the crate's statements in the plan gate's registry.
pub const fn link() {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_graph_without_a_bus_reports_nothing_and_does_not_panic() {
        let graph = Graph::new();
        graph.announce(|| graph::Requeued { requeued: 1 });
    }

    #[tokio::test]
    async fn a_graph_with_a_bus_announces_on_it() {
        let graph = Graph::new();
        let bus = EventBus::new(4);
        let mut rx = bus.subscribe();
        graph.events.set(bus).unwrap();

        graph.announce(|| graph::Requeued { requeued: 1 });

        assert_eq!(rx.try_recv().unwrap().event.name(), "graph.requeued");
    }

    struct Backlogged;

    impl Actor for Backlogged {
        type Msg = GraphMsg;
        type State = ();
        type Arguments = ();

        async fn pre_start(
            &self,
            _myself: ActorRef<GraphMsg>,
            _args: (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            Ok(())
        }

        async fn handle(
            &self,
            _myself: ActorRef<GraphMsg>,
            msg: GraphMsg,
            _state: &mut (),
        ) -> Result<(), ractor::ActorProcessingErr> {
            if let GraphMsg::UpstreamProbed(_, reply) = msg {
                tokio::time::sleep(std::time::Duration::from_secs(3600)).await;
                let _ = reply.send(Ok(()));
            }
            Ok(())
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_caller_behind_a_backlog_gets_the_answer_the_actor_still_gives() {
        let graph = Graph::new();
        let (writer, _) = Actor::spawn(None, Backlogged, ()).await.unwrap();
        graph.writer.send_replace(Some(writer.clone()));

        graph
            .upstream_probed(vec![gradient_types::DerivationId::now_v7()])
            .await
            .unwrap();

        writer.stop(None);
    }

    #[derive(Clone, Default)]
    struct WarningEventFieldTextSubscriber(Arc<std::sync::Mutex<Vec<String>>>);

    impl tracing::Subscriber for WarningEventFieldTextSubscriber {
        fn enabled(&self, _: &tracing::Metadata<'_>) -> bool {
            true
        }

        fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
            tracing::span::Id::from_u64(1)
        }

        fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

        fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

        fn event(&self, event: &tracing::Event<'_>) {
            struct Fields(Vec<String>);
            impl tracing::field::Visit for Fields {
                fn record_debug(
                    &mut self,
                    field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    self.0.push(format!("{}={value:?}", field.name()));
                }
            }
            if *event.metadata().level() == tracing::Level::WARN {
                let mut fields = Fields(Vec::new());
                event.record(&mut fields);
                self.0.lock().unwrap().push(fields.0.join(" "));
            }
        }

        fn enter(&self, _: &tracing::span::Id) {}

        fn exit(&self, _: &tracing::span::Id) {}
    }

    #[test]
    fn test_failed_page_warnings_show_at_most_twenty_parent_builds_and_error_kind_and_sqlstate_without_database_error_text()
     {
        let graph = Graph::new();
        let page: Vec<DerivationId> = (0..25).map(|_| DerivationId::now_v7()).collect();
        let database_error_text = "duplicate key value (/nix/store/aaaa-other-project-output)";
        let warnings = WarningEventFieldTextSubscriber::default();
        tracing::subscriber::with_default(warnings.clone(), || {
            let mut round = DependencyOutputPublishRoundResult::default();
            graph.log_failed_publish_page_and_decide_round_next_step(
                FailedPublishRoundPage {
                    page: page[..1].to_vec(),
                    published_parent_builds: page[..1].to_vec(),
                    error: anyhow::anyhow!(database_error_text),
                },
                1,
                &mut round,
            );
            graph.log_failed_publish_page_and_decide_round_next_step(
                FailedPublishRoundPage {
                    page: page.clone(),
                    published_parent_builds: page.clone(),
                    error: anyhow::anyhow!(database_error_text),
                },
                25,
                &mut round,
            );
        });

        assert_eq!(
            *warnings.0.lock().unwrap(),
            vec![
                format!(
                    "message=publish of dependency outputs: failure in a page of one parent build. Set this parent build aside for a waiting period. Skip it and move on. parent_build={:?} set_aside_seconds=600 error_kind=\"not a database error\" sqlstate=None",
                    page[0]
                ),
                format!(
                    "message=publish of dependency outputs: page failed, retrying from the same cursor with a smaller page parent_builds={:?} parent_build_count=25 parent_builds_per_page=12 error_kind=\"not a database error\" sqlstate=None",
                    &page[..20]
                ),
            ]
        );
    }
}

#[cfg(test)]
pub(crate) mod test_ctx {
    use gradient_db::{DbContext, WorkerDb};

    pub(crate) use crate::messages::{RecordBatch as Batch, RecordReport as BatchReport};
    pub(crate) use crate::testing::{ctx_with_crypt_file, secret_file, signing_key};

    pub(crate) async fn send(graph: &crate::Graph, batch: Batch) -> anyhow::Result<BatchReport> {
        graph.record(batch).await
    }

    pub(crate) async fn apply_in(scoped: &DbContext, batch: &Batch) -> anyhow::Result<BatchReport> {
        crate::record::apply_batch(scoped, batch).await
    }

    pub(crate) async fn claim_build(
        db: &WorkerDb,
        assignment: gradient_entity::dispatched_job::Model,
        shared_build: gradient_types::ids::DerivationBuildId,
        output_names_to_substitute: Option<Vec<String>>,
    ) -> bool {
        use gradient_db::scheduling::assignment_record::{ClaimGate, claim_assignment};

        let build = ClaimGate::Build {
            shared_build,
            output_names_to_substitute,
        };
        claim_assignment(db, assignment, build)
            .await
            .expect("the claim runs")
    }

    pub(crate) fn worker_loss(
        shared_builds: Vec<gradient_types::ids::DerivationBuildId>,
        cause: crate::OrphanCause,
    ) -> crate::Transition {
        crate::Transition::OrphanedBuilds {
            shared_builds,
            cause,
            observed_peak_ram_mb: Default::default(),
        }
    }
}
