/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Writing one worker batch of discovered derivations into the graph: the rows,
//! the anchors, and the per-evaluation dependency-edge accumulator.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow};
use gradient_db::{
    DbContext, WorkerDb, record_evaluation_message, update_evaluation_status_with_error,
};
use gradient_entity::StorePath;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_types::proto::DiscoveredDerivation;
use gradient_types::*;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter,
};
use tracing::{debug, error, info, warn};

use crate::messages::{IngestBatch, IngestReport, UpstreamHit};

const BATCH_SIZE: usize = 1000;

/// New derivation rows and their outputs, ready for bulk DB insert.
struct DerivationInsertBatch {
    /// Mapping from drv_path to assigned UUID for all derivations (new + pre-existing).
    drv_path_to_id: HashMap<String, DerivationId>,
    new_derivations: Vec<ADerivation>,
    new_outputs: Vec<ADerivationOutput>,
}

impl DerivationInsertBatch {
    /// Build insert rows for derivations not yet in `existing`.
    fn prepare(derivations: &[DiscoveredDerivation], existing: &[MDerivation]) -> Self {
        let mut drv_path_to_id: HashMap<String, DerivationId> =
            existing.iter().map(|d| (d.drv_path(), d.id)).collect();

        let now = gradient_types::now();
        let mut new_derivations: Vec<ADerivation> = Vec::new();
        let mut new_outputs: Vec<ADerivationOutput> = Vec::new();

        for d in derivations {
            if drv_path_to_id.contains_key(&d.drv_path) {
                continue;
            }

            let id = DerivationId::now_v7();
            drv_path_to_id.insert(d.drv_path.clone(), id);
            let (drv_hash, drv_name) = drv_hash_name(&d.drv_path)
                .unwrap_or_else(|| ("unknown".to_owned(), d.drv_path.clone()));
            new_derivations.push(
                MDerivation {
                    id,
                    hash: drv_hash,
                    name: drv_name,
                    architecture: d.architecture.clone(),
                    pname: d.pname.clone(),
                    prefer_local_build: d.prefer_local_build,
                    is_fixed_output: d.is_fixed_output,
                    allow_substitutes: d.allow_substitutes,
                    created_at: now,
                    ..Default::default()
                }
                .into_active_model(),
            );

            for output in &d.outputs {
                let (hash, package) = output_hash_name(&output.path).unwrap_or_else(|| {
                    (
                        gradient_entity::derivation_output::UNKNOWN_OUTPUT_HASH.to_owned(),
                        output.name.clone(),
                    )
                });
                new_outputs.push(
                    MDerivationOutput {
                        id: DerivationOutputId::now_v7(),
                        derivation: id,
                        name: output.name.clone(),
                        hash,
                        package,
                        created_at: now,
                        ..Default::default()
                    }
                    .into_active_model(),
                );
            }
        }

        Self {
            drv_path_to_id,
            new_derivations,
            new_outputs,
        }
    }

    /// Insert new derivations and outputs, returning the `drv_path_to_id` map.
    async fn insert(self, db: &WorkerDb) -> Result<HashMap<String, DerivationId>> {
        for chunk in self.new_derivations.chunks(BATCH_SIZE) {
            if let Err(e) = EDerivation::insert_many(chunk.to_vec()).exec(db).await {
                error!(error = %e, "failed to insert derivations");
                return Err(anyhow!("failed to insert derivations: {e}"));
            }
        }

        for chunk in self.new_outputs.chunks(BATCH_SIZE) {
            if let Err(e) = EDerivationOutput::insert_many(chunk.to_vec())
                .exec(db)
                .await
            {
                error!(error = %e, "failed to insert derivation outputs");
            }
        }

        Ok(self.drv_path_to_id)
    }
}

/// Writes a single batch of discovered derivations, inside the actor's
/// transaction. Holds what every step shares: the scoped context and the
/// evaluation the batch belongs to.
struct BatchWriter<'a> {
    ctx: &'a DbContext,
    evaluation_id: EvaluationId,
}

impl BatchWriter<'_> {
    /// Load derivations that already exist in the DB so we don't re-insert them.
    ///
    /// Filters by `hash` only (Nix store hashes are content-addressed, so
    /// `(project, hash)` is unique in practice) to keep the IN clause
    /// bounded by the number of distinct hashes rather than full drv paths.
    async fn load_existing_derivations(
        &self,
        derivations: &[DiscoveredDerivation],
    ) -> Result<Vec<MDerivation>> {
        let hashes: Vec<String> = derivations
            .iter()
            .filter_map(|d| drv_hash_name(&d.drv_path).map(|(h, _)| h))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        if hashes.is_empty() {
            return Ok(vec![]);
        }

        let db = &self.ctx.worker_db;
        gradient_db::fetch_in_chunks(&hashes, |chunk| async move {
            EDerivation::find()
                .filter(CDerivation::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("query existing derivations")
    }

    /// Persist each derivation's `inputSrcs`: build-time source paths (e.g.
    /// `builtins.toFile` configs) that have no producing derivation. Idempotent
    /// on `(derivation, hash)` so a re-seen derivation backfills its sources
    /// without duplicating. The readiness gate requires every source cached
    /// before a non-substitutable build dispatches, so a source the eval has not
    /// pushed yet holds the build instead of letting it dispatch input-blind and
    /// fail `InputsUnavailable`.
    async fn persist_input_sources(
        &self,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) {
        let now = gradient_types::now();
        let mut rows: Vec<ADerivationInputSource> = Vec::new();
        let mut seen: HashSet<(DerivationId, String)> = HashSet::new();
        for d in derivations {
            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };

            for src in &d.input_sources {
                let Ok(store_path) = StorePath::parse(src) else {
                    continue;
                };

                let hash = store_path.hash().to_owned();
                if !seen.insert((drv_id, hash.clone())) {
                    continue;
                }

                rows.push(
                    MDerivationInputSource {
                        id: DerivationInputSourceId::now_v7(),
                        derivation: drv_id,
                        hash,
                        store_path,
                        created_at: now,
                    }
                    .into_active_model(),
                );
            }
        }

        for chunk in rows.chunks(BATCH_SIZE) {
            let res = EDerivationInputSource::insert_many(chunk.to_vec())
                .on_conflict(
                    sea_orm::sea_query::OnConflict::columns([
                        CDerivationInputSource::Derivation,
                        CDerivationInputSource::Hash,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec(&self.ctx.worker_db)
                .await;
            if let Err(e) = res
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                error!(error = %e, "failed to insert derivation input sources");
            }
        }
    }

    /// Persist the scheduler's narinfo hits onto every `derivation_output` row
    /// sharing the hash, so the lookup runs once and the worker downloads
    /// straight from that upstream URL.
    async fn persist_upstream_hits(&self, hits: &HashMap<String, UpstreamHit>) {
        if hits.is_empty() {
            return;
        }

        let hashes: Vec<String> = hits.keys().cloned().collect();
        let db = &self.ctx.worker_db;
        let outputs = match gradient_db::fetch_in_chunks(&hashes, |chunk| async move {
            EDerivationOutput::find()
                .filter(CDerivationOutput::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await
        {
            Ok(outputs) => outputs,
            Err(e) => {
                error!(error = %e, "failed to load outputs for upstream hits");
                return;
            }
        };

        for o in outputs.iter().filter(|o| !o.is_cached_anywhere()) {
            let Some(hit) = hits.get(&o.hash) else {
                continue;
            };

            let mut am = o.clone().into_active_model();
            am.external_url = Set(hit.url.clone());
            am.nar_hash = Set(hit.nar_hash.clone());
            am.file_hash = Set(hit.file_hash.clone());
            am.file_size = Set(hit.file_size);
            am.references = Set(hit.references.clone());
            am.deriver = Set(hit.deriver.clone());
            if o.nar_size.is_none() {
                am.nar_size = Set(hit.nar_size);
            }

            if o.ca.is_none() {
                am.ca = Set(hit.ca.clone());
            }

            if let Err(e) = am.update(db).await {
                error!(hash = %o.hash, error = %e, "failed to persist upstream availability");
            }
        }
    }

    /// Upsert the global `derivation_build` anchor for each discovered
    /// derivation. Build-once: `ON CONFLICT (derivation) DO NOTHING` leaves any
    /// existing anchor (from a prior eval) untouched, so a derivation builds at
    /// most once across all evaluations. No per-eval build rows, no `via`.
    async fn resolve_anchors(
        &self,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
        batch: &IngestBatch,
    ) -> Result<()> {
        let now = gradient_types::now();

        let all_drv_ids: Vec<DerivationId> = derivations
            .iter()
            .filter_map(|d| drv_path_to_id.get(&d.drv_path).copied())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let upstream_ids: HashSet<DerivationId> = derivations
            .iter()
            .filter(|d| batch.upstream_substitutable.contains(&d.drv_path))
            .filter_map(|d| drv_path_to_id.get(&d.drv_path).copied())
            .collect();

        let mut anchors: Vec<ADerivationBuild> = Vec::new();
        let mut seen: HashSet<DerivationId> = HashSet::new();
        for d in derivations {
            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };
            if !seen.insert(drv_id) {
                continue;
            }

            let is_truly_substituted = batch.truly_substituted.contains(&d.drv_path);
            let (status, substitutable) = if is_truly_substituted {
                (BuildStatus::Substituted, false)
            } else if batch.upstream_substitutable.contains(&d.drv_path) {
                (BuildStatus::Created, true)
            } else {
                (BuildStatus::Created, d.substituted)
            };

            anchors.push(
                MDerivationBuild {
                    id: DerivationBuildId::now_v7(),
                    derivation: drv_id,
                    status,
                    substitutable,
                    substituted: matches!(status, BuildStatus::Substituted),
                    // Truly-substituted means every output is closure-complete in our
                    // cache, so the anchor satisfies its dependents' dispatch gate now.
                    closure_complete: is_truly_substituted,
                    timeout_secs: d.timeout_secs.map(|v| v as i64),
                    max_silent_secs: d.max_silent_secs.map(|v| v as i64),
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                }
                .into_active_model(),
            );
        }

        for chunk in anchors.chunks(BATCH_SIZE) {
            let res = EDerivationBuild::insert_many(chunk.to_vec())
                .on_conflict(
                    sea_orm::sea_query::OnConflict::column(CDerivationBuild::Derivation)
                        .do_nothing()
                        .to_owned(),
                )
                .exec(&self.ctx.worker_db)
                .await;
            if let Err(e) = res
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                error!(error = %e, "failed to upsert derivation_build anchors");
                return Err(anyhow!("failed to upsert anchors: {e}"));
            }
        }

        // Per-eval build_job rows: one per (evaluation, derivation), linking the
        // eval to the shared anchor. These are the per-eval "builds" the UI and
        // CI reactor see; the anchor holds the actual build state.
        let db = &self.ctx.worker_db;
        let anchor_by_drv: HashMap<DerivationId, DerivationBuildId> =
            gradient_db::fetch_in_chunks(&all_drv_ids, |chunk| async move {
                EDerivationBuild::find()
                    .filter(CDerivationBuild::Derivation.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|a| (a.derivation, a.id))
            .collect();

        let mut jobs: Vec<ABuildJob> = Vec::new();
        for &drv_id in &all_drv_ids {
            if let Some(&anchor_id) = anchor_by_drv.get(&drv_id) {
                jobs.push(
                    MBuildJob {
                        id: gradient_types::ids::BuildJobId::now_v7(),
                        evaluation: self.evaluation_id,
                        derivation: drv_id,
                        derivation_build: anchor_id,
                        score: 0.0,
                        score_breakdown: serde_json::json!({}),
                        created_at: now,
                    }
                    .into_active_model(),
                );
            }
        }

        for chunk in jobs.chunks(BATCH_SIZE) {
            let res = EBuildJob::insert_many(chunk.to_vec())
                .on_conflict(
                    sea_orm::sea_query::OnConflict::columns([
                        CBuildJob::Evaluation,
                        CBuildJob::Derivation,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec(&self.ctx.worker_db)
                .await;
            if let Err(e) = res
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                error!(error = %e, "failed to upsert build_job rows");
            }
        }

        // A new evaluation retries anchors a previous eval left terminal-failed:
        // the global anchor's failure is not this eval's verdict (caches/network
        // may have changed). promote_ready then re-queues the reset Created rows.
        if let Err(e) = gradient_db::requeue_failed_anchors(db, &all_drv_ids).await {
            error!(error = %e, "failed to re-queue failed anchors for new eval");
        }

        // `ON CONFLICT DO NOTHING` leaves existing build-once anchors untouched,
        // so flip not-yet-succeeded ones to substitutable when an upstream now
        // offers the output: a previously-built/failed derivation substitutes
        // instead of rebuilding (its fetcher origin may have rotted).
        if !upstream_ids.is_empty() {
            let ids: Vec<DerivationId> = upstream_ids.iter().copied().collect();
            if let Err(e) = gradient_db::for_each_chunk(&ids, |chunk| async move {
                EDerivationBuild::update_many()
                    .col_expr(
                        CDerivationBuild::Substitutable,
                        sea_orm::sea_query::Expr::value(true),
                    )
                    .filter(CDerivationBuild::Derivation.is_in(chunk))
                    .filter(CDerivationBuild::Status.is_not_in([
                        i32::from(BuildStatus::Completed),
                        i32::from(BuildStatus::Substituted),
                    ]))
                    .exec(db)
                    .await
            })
            .await
            {
                error!(error = %e, "failed to flag existing anchors substitutable from upstream");
            }
        }

        // Conversely, clear the flag on not-yet-succeeded anchors no upstream
        // offers this eval. A stale `substitutable=true` would otherwise let the
        // anchor bypass the dependency gate and dispatch a substitute that
        // escalates into a build whose closure was never produced.
        let not_upstream: Vec<DerivationId> = all_drv_ids
            .iter()
            .copied()
            .filter(|d| !upstream_ids.contains(d))
            .collect();
        if !not_upstream.is_empty()
            && let Err(e) = gradient_db::for_each_chunk(&not_upstream, |chunk| async move {
                EDerivationBuild::update_many()
                    .col_expr(
                        CDerivationBuild::Substitutable,
                        sea_orm::sea_query::Expr::value(false),
                    )
                    .filter(CDerivationBuild::Derivation.is_in(chunk))
                    .filter(CDerivationBuild::Substitutable.eq(true))
                    .filter(CDerivationBuild::Status.is_not_in([
                        i32::from(BuildStatus::Completed),
                        i32::from(BuildStatus::Substituted),
                    ]))
                    .exec(db)
                    .await
            })
            .await
        {
            error!(error = %e, "failed to clear stale substitutable flags");
        }

        Ok(())
    }

    /// Record per-derivation system-feature requirements in the DB.
    async fn add_system_features(
        &self,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) {
        for d in derivations {
            if d.required_features.is_empty() {
                continue;
            }

            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };

            if let Err(e) = gradient_db::add_features(
                self.ctx,
                d.required_features.clone(),
                gradient_entity::feature::FeatureKind::Feature,
                Some(drv_id),
            )
            .await
            {
                error!(error = %e, %drv_id, "failed to add system features");
            }
        }
    }

    /// Persist Nix evaluation warnings and errors as evaluation messages.
    async fn record_eval_messages(&self, warnings: &[String], errors: &[String]) {
        for warning in warnings {
            record_evaluation_message(
                self.ctx,
                self.evaluation_id,
                MessageLevel::Warning,
                warning.clone(),
                Some("nix-eval".to_string()),
            )
            .await;
        }

        for error in errors {
            record_evaluation_message(
                self.ctx,
                self.evaluation_id,
                MessageLevel::Error,
                error.clone(),
                Some("nix-eval".to_string()),
            )
            .await;
        }
    }

    /// Insert this batch's task entry points, returning their derivation ids so
    /// the caller can announce their current anchor status to the forge.
    async fn process_entry_points(
        &self,
        task_id: TaskId,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) -> Vec<DerivationId> {
        let now = gradient_types::now();

        let mut active_entry_points: Vec<AEntryPoint> = Vec::new();
        let mut entry_point_drvs: Vec<DerivationId> = Vec::new();

        for d in derivations {
            if d.attr.is_empty() {
                continue;
            }

            if let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) {
                entry_point_drvs.push(drv_id);
                active_entry_points.push(
                    MEntryPoint {
                        id: EntryPointId::now_v7(),
                        task: task_id,
                        evaluation: self.evaluation_id,
                        derivation: drv_id,
                        eval: d.attr.clone(),
                        created_at: now,
                        ..Default::default()
                    }
                    .into_active_model(),
                );
            }
        }

        for chunk in active_entry_points.chunks(BATCH_SIZE) {
            if let Err(e) = EEntryPoint::insert_many(chunk.to_vec())
                .exec(&self.ctx.worker_db)
                .await
            {
                error!(error = %e, "failed to insert entry points");
            }
        }

        entry_point_drvs
    }
}

/// Only a streaming evaluation takes batches. Anything else is a stale
/// dispatch (a worker that died mid-walk, or a re-queued evaluation's old
/// worker) and is dropped, never merged into the live walk's graph.
fn accepts_batches(status: EvaluationStatus) -> bool {
    matches!(
        status,
        EvaluationStatus::Fetching
            | EvaluationStatus::EvaluatingFlake
            | EvaluationStatus::EvaluatingDerivation
    )
}

pub(crate) async fn apply_batch(
    ctx: &DbContext,
    edges: &mut HashMap<EvaluationId, EvalEdgeAccumulator>,
    batch: &IngestBatch,
) -> Result<IngestReport> {
    let evaluation_id = batch.evaluation;
    match EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await
        .context("fetch evaluation")?
    {
        Some(e) if !accepts_batches(e.status) => {
            warn!(%evaluation_id, status = ?e.status, "batch for an evaluation that is not streaming; dropped as stale");
            return Ok(IngestReport {
                evaluation: evaluation_id,
                task: batch.task,
                skipped: true,
                ..Default::default()
            });
        }
        Some(_) => {}
        None => anyhow::bail!("evaluation {evaluation_id} not found"),
    }

    let writer = BatchWriter { ctx, evaluation_id };
    let existing = writer.load_existing_derivations(&batch.derivations).await?;
    let prepared = DerivationInsertBatch::prepare(&batch.derivations, &existing);
    let new_derivations = prepared.new_derivations.len();
    let drv_path_to_id = prepared.insert(&ctx.worker_db).await?;
    writer
        .persist_input_sources(&batch.derivations, &drv_path_to_id)
        .await;
    writer.persist_upstream_hits(&batch.upstream_hits).await;
    writer
        .resolve_anchors(&batch.derivations, &drv_path_to_id, batch)
        .await?;
    writer
        .add_system_features(&batch.derivations, &drv_path_to_id)
        .await;
    writer
        .record_eval_messages(&batch.warnings, &batch.errors)
        .await;
    let entry_points = match batch.task {
        Some(task) => {
            writer
                .process_entry_points(task, &batch.derivations, &drv_path_to_id)
                .await
        }
        None => Vec::new(),
    };

    let acc = edges.entry(evaluation_id).or_default();
    acc.add_batch(&batch.derivations);
    if let Err(e) = flush_ready_edges(&ctx.worker_db, evaluation_id, acc).await {
        warn!(error = %e, %evaluation_id, "mid-stream edge flush failed; deferred to completion");
    }

    Ok(IngestReport {
        evaluation: evaluation_id,
        task: batch.task,
        skipped: false,
        new_derivations,
        entry_points,
    })
}

/// What a landed batch triggers outside its transaction: forge checks for the
/// entry points, the per-task evaluation GC, and the live-channel ping.
pub(crate) async fn after_commit(ctx: &DbContext, batch: &IngestBatch, report: &IngestReport) {
    if report.skipped {
        return;
    }

    if let Some(task_id) = batch.task {
        gradient_db::announce_entry_point_statuses(ctx, report.evaluation, &report.entry_points)
            .await;
        if let Ok(Some(task)) = ETask::find_by_id(task_id).one(&ctx.worker_db).await {
            let gc_ctx = ctx.detached();
            let keep = task.keep_evaluations as usize;
            ctx.shutdown.spawn(async move {
                if let Err(e) = gradient_db::gc_task_evaluations(&gc_ctx, task_id, keep).await {
                    error!(error = %e, %task_id, "GC: per-task evaluation GC failed");
                }
            });
        }
    }

    let _ = ctx.board_events.send(BoardEvent::EvaluationProgress {
        task: batch.task.map(|t| t.into_inner()),
        evaluation_id: report.evaluation.into_inner(),
    });
}

pub(crate) async fn fail_evaluation(ctx: &DbContext, evaluation: EvaluationId, message: &str) {
    if let Ok(Some(eval)) = EEvaluation::find_by_id(evaluation)
        .one(&ctx.worker_db)
        .await
    {
        update_evaluation_status_with_error(
            ctx,
            eval,
            EvaluationStatus::Failed,
            message.to_owned(),
            Some("db-insert".to_string()),
        )
        .await;
    }
}

fn drv_hash_name(drv_path: &str) -> Option<(String, String)> {
    let sp = StorePath::parse(drv_path).ok()?;
    let name = sp.name().strip_suffix(".drv")?;
    Some((sp.hash().to_owned(), name.to_owned()))
}

fn output_hash_name(path: &str) -> Option<(String, String)> {
    let sp = StorePath::parse(path).ok()?;
    Some((sp.hash().to_owned(), sp.name().to_owned()))
}

/// Per-evaluation accumulator of discovered dependency edges, resolved
/// incrementally as batches stream in. A `(src, deps)` pair leaves `pending`
/// the moment its full edge set is recorded (see [`flush_ready_edges`]); the
/// remainder is settled by [`flush_deferred_deps`] at stream completion, which
/// alone may flag `edges_unresolved`: mid-stream, an unknown dep may simply
/// not have streamed yet.
#[derive(Default)]
pub(crate) struct EvalEdgeAccumulator {
    /// Canonical drv_path to recorded derivation id, learned from DB lookups.
    known: HashMap<String, DerivationId>,
    /// Paths already queried and absent; re-queried only after their own batch
    /// arrives (`add_batch` unmarks them), so an absent dep costs one lookup.
    missing: HashSet<String>,
    /// Pairs whose edge set is not yet fully recorded.
    pending: EdgePairs,
    /// This stream's zero-dep drv_paths, trivially `edges_complete` once their
    /// anchor row exists.
    leaves: Vec<String>,
}

impl EvalEdgeAccumulator {
    pub(crate) fn add_batch(&mut self, derivations: &[DiscoveredDerivation]) {
        for d in derivations {
            self.missing.remove(&d.drv_path);
            if d.dependencies.is_empty() {
                self.leaves.push(d.drv_path.clone());
            } else {
                self.pending
                    .push((d.drv_path.clone(), d.dependencies.clone()));
            }
        }
    }

    pub(crate) fn into_pending(self) -> EdgePairs {
        self.pending
    }
}

/// Record every dependency edge resolvable right now and mark the fully
/// resolved sources (plus the stream's zero-dep leaves) `edges_complete`, so
/// the dispatch tick's promotion pipeline can queue and dispatch them while
/// the eval stream is still running instead of waiting for the completion
/// flush. Runs after each persisted batch; safety is unchanged: promotion
/// still requires the full readiness gate and dispatch additionally gates on
/// `drv_closure_cached` / cached input sources, so nothing dispatches before
/// its inputs are importable.
async fn flush_ready_edges(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
    acc: &mut EvalEdgeAccumulator,
) -> Result<()> {
    let mut lookup: HashSet<String> = HashSet::new();
    for (src, deps) in &acc.pending {
        for p in std::iter::once(src).chain(deps.iter()) {
            if !acc.known.contains_key(p) && !acc.missing.contains(p) {
                lookup.insert(p.clone());
            }
        }
    }

    for p in &acc.leaves {
        if !acc.known.contains_key(p) && !acc.missing.contains(p) {
            lookup.insert(p.clone());
        }
    }

    if !lookup.is_empty() {
        let hashes: Vec<String> = lookup
            .iter()
            .filter_map(|p| drv_hash_name(p).map(|(h, _)| h))
            .collect();
        let found: HashMap<String, DerivationId> =
            gradient_db::fetch_in_chunks(&hashes, |chunk| async move {
                EDerivation::find()
                    .filter(CDerivation::Hash.is_in(chunk))
                    .all(db)
                    .await
            })
            .await
            .context("flush_ready_edges: query derivations")?
            .into_iter()
            .map(|d| (d.drv_path(), d.id))
            .collect();

        for p in lookup {
            match found.get(&p) {
                Some(&id) => {
                    acc.known.insert(p, id);
                }
                None => {
                    acc.missing.insert(p);
                }
            }
        }
    }

    let (ready, still_pending) =
        partition_ready_edges(std::mem::take(&mut acc.pending), &acc.known);
    acc.pending = still_pending;
    let mut complete: Vec<DerivationId> = ready.iter().map(|(src, _)| acc.known[src]).collect();
    let mut leaves_left = Vec::new();
    for p in acc.leaves.drain(..) {
        match acc.known.get(&p) {
            Some(&id) => complete.push(id),
            None => leaves_left.push(p),
        }
    }

    acc.leaves = leaves_left;
    if ready.is_empty() && complete.is_empty() {
        return Ok(());
    }

    let known = &acc.known;
    let edges: Vec<ADerivationDependency> = ready
        .iter()
        .flat_map(|(src, deps)| {
            let src_id = known[src];
            deps.iter().map(move |dep| {
                MDerivationDependency {
                    derivation: src_id,
                    dependency: known[dep],
                }
                .into_active_model()
            })
        })
        .collect();

    for chunk in edges.chunks(BATCH_SIZE) {
        if let Err(e) = EDerivationDependency::insert_many(chunk.to_vec())
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    CDerivationDependency::Derivation,
                    CDerivationDependency::Dependency,
                ])
                .do_nothing()
                .to_owned(),
            )
            .try_insert()
            .exec(db)
            .await
        {
            error!(error = %e, "flush_ready_edges: failed to insert edges; deferring to completion flush");
            acc.pending.extend(ready);
            return Ok(());
        }
    }

    if let Err(e) = gradient_db::for_each_chunk(&complete, |chunk| async move {
        EDerivationBuild::update_many()
            .col_expr(
                CDerivationBuild::EdgesComplete,
                sea_orm::sea_query::Expr::value(true),
            )
            .col_expr(
                CDerivationBuild::EdgesUnresolved,
                sea_orm::sea_query::Expr::value(false),
            )
            .filter(CDerivationBuild::Derivation.is_in(chunk))
            .exec(db)
            .await
    })
    .await
    {
        error!(error = %e, "flush_ready_edges: failed to mark edges_complete");
    }

    debug!(
        %evaluation_id,
        inserted = edges.len(),
        edges_complete = complete.len(),
        pending = acc.pending.len(),
        "flushed ready dependency edges mid-stream"
    );
    Ok(())
}

/// Discovered `(drv_path, dependency drv_paths)` pairs awaiting edge insertion.
pub(crate) type EdgePairs = Vec<(String, Vec<String>)>;

/// Split pending pairs into the fully resolvable (source and every dep known)
/// and the remainder. Pairs stay as string pairs so a failed insert can push
/// them back for the completion flush.
fn partition_ready_edges(
    pending: EdgePairs,
    known: &HashMap<String, DerivationId>,
) -> (EdgePairs, EdgePairs) {
    pending.into_iter().partition(|(src, deps)| {
        known.contains_key(src) && deps.iter().all(|d| known.contains_key(d))
    })
}

pub(crate) async fn flush_deferred_deps(
    db: &WorkerDb,
    evaluation_id: EvaluationId,
    deferred: EdgePairs,
) -> Result<()> {
    if deferred.is_empty() {
        return Ok(());
    }

    // Hashes are content-addressed (32-char nix32), so filtering by hash alone
    // pins a row down in the global derivation graph.
    let mut all_paths: HashSet<String> = HashSet::new();
    for (src, deps) in &deferred {
        all_paths.insert(src.clone());
        for d in deps {
            all_paths.insert(d.clone());
        }
    }

    let all_hashes: Vec<String> = all_paths
        .iter()
        .filter_map(|p| drv_hash_name(p).map(|(h, _)| h))
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    let drv_path_to_id: HashMap<String, DerivationId> =
        gradient_db::fetch_in_chunks(&all_hashes, |chunk| async move {
            EDerivation::find()
                .filter(CDerivation::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("flush_deferred_deps: query derivations")?
        .into_iter()
        .map(|d| (d.drv_path(), d.id))
        .collect();

    let (edge_pairs, resolved_sources, unresolved_sources) =
        resolve_deferred_edges(&deferred, &drv_path_to_id);

    let edges: Vec<ADerivationDependency> = edge_pairs
        .iter()
        .map(|(src, dep)| {
            MDerivationDependency {
                derivation: *src,
                dependency: *dep,
            }
            .into_active_model()
        })
        .collect();

    for chunk in edges.chunks(BATCH_SIZE) {
        if let Err(e) = EDerivationDependency::insert_many(chunk.to_vec())
            .on_conflict(
                sea_orm::sea_query::OnConflict::columns([
                    CDerivationDependency::Derivation,
                    CDerivationDependency::Dependency,
                ])
                .do_nothing()
                .to_owned(),
            )
            .try_insert()
            .exec(db)
            .await
        {
            error!(error = %e, "flush_deferred_deps: failed to insert edges");
        }
    }

    // Persist per-source resolution so `mark_edges_complete_for_eval` refuses to
    // promote an anchor whose declared edge set is incomplete (a dependency this
    // eval never recorded), and clears the flag once a later eval resolves them.
    set_edges_unresolved(db, &unresolved_sources, true).await;
    set_edges_unresolved(db, &resolved_sources, false).await;

    info!(
        %evaluation_id,
        inserted = edges.len(),
        unresolved = unresolved_sources.len(),
        "flushed deferred dependency edges"
    );
    Ok(())
}

/// Resolve deferred `(src, [dep])` drv-path pairs against the recorded
/// derivations. Returns the resolvable edges, the sources whose every dep
/// resolved, and the sources with at least one unresolved dep (a dependency the
/// eval never recorded, whose edge is dropped, so the source must be held off
/// promotion rather than dispatched as dependency-free).
fn resolve_deferred_edges(
    deferred: &[(String, Vec<String>)],
    drv_path_to_id: &HashMap<String, DerivationId>,
) -> (
    Vec<(DerivationId, DerivationId)>,
    HashSet<DerivationId>,
    HashSet<DerivationId>,
) {
    let mut edges = Vec::new();
    let mut all_sources = HashSet::new();
    let mut unresolved = HashSet::new();
    for (src, deps) in deferred {
        let Some(&src_id) = drv_path_to_id.get(src) else {
            continue;
        };

        all_sources.insert(src_id);
        for dep in deps {
            match drv_path_to_id.get(dep) {
                Some(&dep_id) => edges.push((src_id, dep_id)),
                None => {
                    unresolved.insert(src_id);
                }
            }
        }
    }

    let resolved = all_sources.difference(&unresolved).copied().collect();
    (edges, resolved, unresolved)
}

async fn set_edges_unresolved(db: &WorkerDb, ids: &HashSet<DerivationId>, value: bool) {
    if ids.is_empty() {
        return;
    }

    let ids: Vec<DerivationId> = ids.iter().copied().collect();
    if let Err(e) = gradient_db::for_each_chunk(&ids, |chunk| async move {
        EDerivationBuild::update_many()
            .col_expr(
                CDerivationBuild::EdgesUnresolved,
                sea_orm::sea_query::Expr::value(value),
            )
            .filter(CDerivationBuild::Derivation.is_in(chunk))
            .exec(db)
            .await
    })
    .await
    {
        error!(error = %e, "flush_deferred_deps: failed to update edges_unresolved");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drv(path: &str, deps: &[&str]) -> DiscoveredDerivation {
        DiscoveredDerivation {
            attr: String::new(),
            drv_path: path.to_owned(),
            outputs: vec![],
            dependencies: deps.iter().map(|d| (*d).to_owned()).collect(),
            input_sources: vec![],
            architecture: "x86_64-linux".to_owned(),
            required_features: vec![],
            timeout_secs: None,
            max_silent_secs: None,
            prefer_local_build: false,
            is_fixed_output: false,
            allow_substitutes: true,
            pname: None,
            substituted: false,
        }
    }

    /// A source with an unrecorded dependency is flagged unresolved (so it's held
    /// off promotion) and excluded from the resolved set; a fully-resolved source
    /// is the opposite. This is what keeps a 0-edge build_job whose edge was
    /// dropped from being marked `edges_complete` and dispatched dependency-free.
    #[test]
    fn deferred_edges_flag_sources_with_unrecorded_deps() {
        let src = DerivationId::now_v7();
        let dep = DerivationId::now_v7();
        let mut map = HashMap::new();
        map.insert("src.drv".to_string(), src);
        map.insert("dep.drv".to_string(), dep);

        let deferred = vec![(
            "src.drv".to_string(),
            vec!["dep.drv".to_string(), "missing.drv".to_string()],
        )];
        let (edges, resolved, unresolved) = resolve_deferred_edges(&deferred, &map);
        assert_eq!(edges, vec![(src, dep)]);
        assert!(unresolved.contains(&src), "unrecorded dep flags the source");
        assert!(!resolved.contains(&src), "and excludes it from resolved");

        let ok = vec![("src.drv".to_string(), vec!["dep.drv".to_string()])];
        let (_, resolved, unresolved) = resolve_deferred_edges(&ok, &map);
        assert!(resolved.contains(&src));
        assert!(unresolved.is_empty());
    }

    /// A pair is only ready when its source AND every dep have recorded rows;
    /// anything else stays pending for a later batch or the completion flush.
    /// Mid-stream, an unknown dep must never be treated as unresolvable: it
    /// may simply not have streamed yet.
    #[test]
    fn partition_holds_pairs_with_unknown_paths() {
        let id_a = DerivationId::now_v7();
        let id_b = DerivationId::now_v7();
        let known: HashMap<String, DerivationId> =
            [("a.drv".to_owned(), id_a), ("b.drv".to_owned(), id_b)].into();

        let pending = vec![
            ("a.drv".to_owned(), vec!["b.drv".to_owned()]),
            (
                "a.drv".to_owned(),
                vec!["b.drv".to_owned(), "later.drv".to_owned()],
            ),
            ("unknown-src.drv".to_owned(), vec!["b.drv".to_owned()]),
        ];
        let (ready, still) = partition_ready_edges(pending, &known);
        assert_eq!(ready, vec![("a.drv".to_owned(), vec!["b.drv".to_owned()])]);
        assert_eq!(
            still.len(),
            2,
            "unknown src or dep stays pending: {still:?}"
        );
    }

    /// A dep first queried-and-missing must become resolvable once its own
    /// batch arrives: `add_batch` unmarks it so the next flush re-queries.
    #[test]
    fn add_batch_unmarks_missing_and_splits_leaves() {
        let mut acc = EvalEdgeAccumulator::default();
        acc.missing.insert("leaf.drv".to_owned());

        acc.add_batch(&[drv("leaf.drv", &[]), drv("root.drv", &["leaf.drv"])]);

        assert!(
            !acc.missing.contains("leaf.drv"),
            "batch arrival must clear the DB-miss memo"
        );
        assert_eq!(acc.leaves, vec!["leaf.drv".to_owned()]);
        assert_eq!(
            acc.pending,
            vec![("root.drv".to_owned(), vec!["leaf.drv".to_owned()])]
        );
        assert_eq!(
            acc.into_pending(),
            vec![("root.drv".to_owned(), vec!["leaf.drv".to_owned()])]
        );
    }
}
