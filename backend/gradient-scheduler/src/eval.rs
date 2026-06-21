/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Handles `EvalResult` messages from workers.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_db::{
    record_evaluation_message, update_evaluation_status, update_evaluation_status_with_error,
};
use gradient_exec::strip_nix_store_prefix;
use gradient_sources::{get_hash_from_path, parse_drv_hash_name};
use gradient_types::*;
use gradient_core::ServerState;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter};
use tracing::{debug, error, info};

use super::build::check_evaluation_done;
use super::jobs::PendingEvalJob;
use gradient_types::proto::DiscoveredDerivation;

const BATCH_SIZE: usize = 1000;

// ── Derivation batch preparation ──────────────────────────────────────────────

/// New derivation rows and their outputs, ready for bulk DB insert.
struct DerivationInsertBatch {
    /// Mapping from drv_path → assigned UUID for all derivations (new + pre-existing).
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
            let (drv_hash, drv_name) = parse_drv_hash_name(&d.drv_path)
                .unwrap_or_else(|_| ("unknown".to_owned(), d.drv_path.clone()));
            new_derivations.push(MDerivation {
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
            }.into_active_model());
            for output in &d.outputs {
                let (hash, package) = get_hash_from_path(output.path.clone())
                    .unwrap_or_else(|_| ("unknown".to_owned(), output.name.clone()));
                new_outputs.push(MDerivationOutput {
                    id: DerivationOutputId::now_v7(),
                    derivation: id,
                    name: output.name.clone(),
                    hash,
                    package,
                    created_at: now,
                    ..Default::default()
                }.into_active_model());
            }
        }

        Self {
            drv_path_to_id,
            new_derivations,
            new_outputs,
        }
    }

    /// Insert new derivations and outputs into the DB.
    ///
    /// Returns the `drv_path_to_id` map for downstream use.
    async fn insert(
        self,
        state: &Arc<ServerState>,
        evaluation: &MEvaluation,
    ) -> Result<HashMap<String, DerivationId>> {
        if !self.new_derivations.is_empty() {
            for chunk in self.new_derivations.chunks(BATCH_SIZE) {
                if let Err(e) = EDerivation::insert_many(chunk.to_vec())
                    .exec(&state.worker_db)
                    .await
                {
                    error!(error = %e, "failed to insert derivations");
                    update_evaluation_status_with_error(
                        &state.db(),
                        evaluation.clone(),
                        EvaluationStatus::Failed,
                        format!("failed to insert derivations: {}", e),
                        Some("db-insert".to_string()),
                    )
                    .await;
                    return Err(e.into());
                }
            }
        }
        if !self.new_outputs.is_empty() {
            for chunk in self.new_outputs.chunks(BATCH_SIZE) {
                if let Err(e) = EDerivationOutput::insert_many(chunk.to_vec())
                    .exec(&state.worker_db)
                    .await
                {
                    error!(error = %e, "failed to insert derivation outputs");
                }
            }
        }
        Ok(self.drv_path_to_id)
    }
}

// ── EvalResultProcessor ───────────────────────────────────────────────────────

/// Processes a single batch of derivations discovered during evaluation.
///
/// Holds the context shared by every step: server state and evaluation
/// identity. Created once in [`handle_eval_result`] and passed through each
/// pipeline stage.
struct EvalResultProcessor<'a> {
    state: &'a Arc<ServerState>,
    evaluation_id: EvaluationId,
    evaluation: MEvaluation,
}

impl<'a> EvalResultProcessor<'a> {
    fn new(
        state: &'a Arc<ServerState>,
        evaluation_id: EvaluationId,
        evaluation: MEvaluation,
    ) -> Self {
        Self {
            state,
            evaluation_id,
            evaluation,
        }
    }

    /// Load derivations that already exist in the DB so we don't re-insert them.
    ///
    /// Filters by `hash` only (Nix store hashes are content-addressed, so
    /// `(organization, hash)` is unique in practice) to keep the IN clause
    /// bounded by the number of distinct hashes rather than full drv paths.
    async fn load_existing_derivations(
        &self,
        derivations: &[DiscoveredDerivation],
    ) -> Result<Vec<MDerivation>> {
        let hashes: Vec<String> = derivations
            .iter()
            .filter_map(|d| parse_drv_hash_name(&d.drv_path).ok().map(|(h, _)| h))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();
        if hashes.is_empty() {
            return Ok(vec![]);
        }

        let db = &self.state.worker_db;
        gradient_db::fetch_in_chunks(&hashes, |chunk| async move {
            EDerivation::find()
                .filter(CDerivation::Hash.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("query existing derivations")
    }

    /// Upsert the global `derivation_build` anchor for each discovered
    /// derivation. Build-once: `ON CONFLICT (derivation) DO NOTHING` leaves any
    /// existing anchor (from a prior eval) untouched, so a derivation builds at
    /// most once across all evaluations. No per-eval build rows, no `via`.
    async fn resolve_anchors(
        &self,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) -> Result<()> {
        let now = gradient_types::now();

        let all_drv_ids: Vec<DerivationId> = derivations
            .iter()
            .filter_map(|d| drv_path_to_id.get(&d.drv_path).copied())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let truly_substituted = self.compute_truly_substituted(&all_drv_ids).await?;
        let not_substituted: Vec<DerivationId> = all_drv_ids
            .iter()
            .copied()
            .filter(|id| !truly_substituted.contains(id))
            .collect();
        let upstream_substitutable = self
            .compute_upstream_substitutable(&not_substituted)
            .await
            .unwrap_or_else(|e| {
                error!(error = %e, "upstream substitutability probe failed");
                std::collections::HashSet::new()
            });

        let mut anchors: Vec<ADerivationBuild> = Vec::new();
        let mut seen: std::collections::HashSet<DerivationId> = std::collections::HashSet::new();
        for d in derivations {
            let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) else {
                continue;
            };
            if !seen.insert(drv_id) {
                continue;
            }

            let (status, substitutable) = if truly_substituted.contains(&drv_id) {
                (BuildStatus::Substituted, false)
            } else if upstream_substitutable.contains(&drv_id) {
                (BuildStatus::Created, true)
            } else {
                (BuildStatus::Created, d.substituted)
            };

            anchors.push(MDerivationBuild {
                id: DerivationBuildId::now_v7(),
                derivation: drv_id,
                status,
                substitutable,
                substituted: matches!(status, BuildStatus::Substituted),
                timeout_secs: d.timeout_secs.map(|v| v as i64),
                max_silent_secs: d.max_silent_secs.map(|v| v as i64),
                prefer_local_build: d.prefer_local_build,
                created_at: now,
                updated_at: now,
                ..Default::default()
            }.into_active_model());
        }

        for chunk in anchors.chunks(BATCH_SIZE) {
            let res = EDerivationBuild::insert_many(chunk.to_vec())
                .on_conflict(
                    sea_orm::sea_query::OnConflict::column(CDerivationBuild::Derivation)
                        .do_nothing()
                        .to_owned(),
                )
                .exec(&self.state.worker_db)
                .await;
            if let Err(e) = res
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                error!(error = %e, "failed to upsert derivation_build anchors");
                update_evaluation_status_with_error(
                    &self.state.db(),
                    self.evaluation.clone(),
                    EvaluationStatus::Failed,
                    format!("failed to upsert anchors: {}", e),
                    Some("db-insert".to_string()),
                )
                .await;
                return Err(e.into());
            }
        }

        // Per-eval build_job rows: one per (evaluation, derivation), linking the
        // eval to the shared anchor. These are the per-eval "builds" the UI and
        // CI reactor see; the anchor holds the actual build state.
        let db = &self.state.worker_db;
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
                .exec(&self.state.worker_db)
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
        if !upstream_substitutable.is_empty() {
            let ids: Vec<DerivationId> = upstream_substitutable.iter().copied().collect();
            if let Err(e) = gradient_db::for_each_chunk(&ids, |chunk| async move {
                EDerivationBuild::update_many()
                    .col_expr(CDerivationBuild::Substitutable, sea_orm::sea_query::Expr::value(true))
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

        // Promotion is deferred to stream completion (`handle_eval_job_completed`),
        // after `flush_deferred_deps` writes the dependency edges. Promoting here
        // would run before any edge exists and queue every anchor regardless of
        // its (not-yet-recorded) dependencies.

        Ok(())
    }

    /// Dispatch `build.substituted` for every just-inserted Substituted build
    /// that has an `entry_point` row. Substituted builds are inserted in
    /// their terminal state and never go through `update_build_status`, so
    /// the regular status-change dispatch path never fires for them.
    ///
    /// Must be called AFTER `process_entry_points` - the reporter skips
    /// build events without an `entry_point`, so dispatching before
    /// `entry_point` rows exist would silently drop every check.
    pub(crate) async fn dispatch_substituted_events(&self) -> Result<(), sea_orm::DbErr> {
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

        let jobs = EBuildJob::find()
            .filter(CBuildJob::Evaluation.eq(self.evaluation_id))
            .all(&self.state.worker_db)
            .await?;
        if jobs.is_empty() {
            return Ok(());
        }

        let db = &self.state.worker_db;
        let anchor_ids: Vec<DerivationBuildId> = jobs.iter().map(|j| j.derivation_build).collect();
        let substituted: std::collections::HashSet<DerivationBuildId> =
            gradient_db::fetch_in_chunks(&anchor_ids, |chunk| async move {
                EDerivationBuild::find()
                    .filter(CDerivationBuild::Id.is_in(chunk))
                    .filter(CDerivationBuild::Status.eq(BuildStatus::Substituted))
                    .all(db)
                    .await
            })
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|a| a.id)
            .collect();

        for job in jobs {
            if substituted.contains(&job.derivation_build) {
                self.state
                    .reactor
                    .on_build_terminal(&self.state.db(), job, BuildStatus::Substituted)
                    .await;
            }
        }
        Ok(())
    }

    /// Return the subset of `drv_ids` whose every `derivation_output` row's
    /// hash matches a `cached_path` with `file_hash IS NOT NULL`. These are
    /// the drvs the server can confidently mark `Substituted` - anything
    /// else must be built (or substituted later when the bytes show up).
    ///
    /// Matching is by hash rather than the explicit
    /// `derivation_output.cached_path` link because that link is set lazily
    /// by `mark_nar_stored` on upload. A new drv whose output hash is
    /// already in `cached_path` (shared FOD source, re-evaluated build
    /// before its first upload, manual cache push) would otherwise be
    /// misclassified as "needs to build" and rerun pointlessly. The
    /// worker-facing `CacheQuery` handler already merges by hash for the
    /// same reason; this brings the eval-time decision in line.
    async fn compute_truly_substituted(
        &self,
        drv_ids: &[DerivationId],
    ) -> Result<std::collections::HashSet<DerivationId>> {
        if drv_ids.is_empty() {
            return Ok(std::collections::HashSet::new());
        }

        let db = &self.state.worker_db;
        let outputs = gradient_db::fetch_in_chunks(drv_ids, |chunk| async move {
            EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("compute_truly_substituted: load derivation_output")?;

        if outputs.is_empty() {
            return Ok(std::collections::HashSet::new());
        }

        let hashes: Vec<String> = outputs
            .iter()
            .map(|o| o.hash.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        let fully_cached_hashes: std::collections::HashSet<String> =
            gradient_db::fetch_in_chunks(&hashes, |chunk| async move {
                ECachedPath::find()
                    .filter(CCachedPath::Hash.is_in(chunk))
                    .all(db)
                    .await
            })
            .await
            .context("compute_truly_substituted: load cached_path")?
            .into_iter()
            .filter(|cp| cp.is_fully_cached())
            .map(|cp| cp.hash)
            .collect();

        let mut outputs_by_drv: HashMap<DerivationId, Vec<&MDerivationOutput>> = HashMap::new();
        for o in &outputs {
            outputs_by_drv.entry(o.derivation).or_default().push(o);
        }

        let mut substituted = std::collections::HashSet::new();
        for (drv_id, outs) in outputs_by_drv {
            let all_present =
                !outs.is_empty() && outs.iter().all(|o| fully_cached_hashes.contains(&o.hash));
            if all_present {
                substituted.insert(drv_id);
            }
        }
        Ok(substituted)
    }

    /// Org-scoped upstream substitutability probe. For derivations not already in
    /// the gradient cache, look up each output's `.narinfo` on the org's
    /// configured upstream caches and persist hits onto `derivation_output`
    /// (`external_url` + narinfo metadata) so the lookup runs once and the worker
    /// downloads directly from that URL. Returns the derivations whose *every*
    /// output is cached somewhere (gradient cache or an upstream) and may
    /// therefore be substituted instead of built.
    async fn compute_upstream_substitutable(
        &self,
        drv_ids: &[DerivationId],
    ) -> Result<std::collections::HashSet<DerivationId>> {
        use std::collections::{HashMap, HashSet};

        if drv_ids.is_empty() {
            return Ok(HashSet::new());
        }

        let db = &self.state.worker_db;
        let Some(org_id) =
            crate::dispatch::organization_id_for_eval(self.state, &self.evaluation).await
        else {
            return Ok(HashSet::new());
        };
        let upstream_urls = gradient_db::upstream_urls_for_org(db, org_id)
            .await
            .unwrap_or_default();
        if upstream_urls.is_empty() {
            return Ok(HashSet::new());
        }

        let outputs = gradient_db::fetch_in_chunks(drv_ids, |chunk| async move {
            EDerivationOutput::find()
                .filter(CDerivationOutput::Derivation.is_in(chunk))
                .all(db)
                .await
        })
        .await
        .context("compute_upstream_substitutable: load derivation_output")?;
        if outputs.is_empty() {
            return Ok(HashSet::new());
        }

        // Outputs not yet available anywhere need a narinfo probe (deduped by hash).
        let to_probe: Vec<(String, String)> = outputs
            .iter()
            .filter(|o| !o.is_cached_anywhere())
            .map(|o| (o.hash.clone(), format!("/nix/store/{}-{}", o.hash, o.package)))
            .collect::<HashMap<_, _>>()
            .into_iter()
            .collect();

        let found = self.probe_upstreams(&upstream_urls, to_probe).await;

        // Persist each hit onto every derivation_output row sharing that hash.
        for o in outputs.iter().filter(|o| !o.is_cached_anywhere()) {
            let Some(cp) = found.get(&o.hash) else {
                continue;
            };
            let mut am = o.clone().into_active_model();
            am.external_url = Set(cp.url.clone());
            am.nar_hash = Set(cp.nar_hash.clone());
            am.file_size = Set(cp.file_size.map(|v| v as i64));
            am.references = Set(cp.references.as_ref().map(|r| r.join(" ")));
            am.deriver = Set(cp.deriver.clone());
            if o.nar_size.is_none() {
                am.nar_size = Set(cp.nar_size.map(|v| v as i64));
            }
            if o.ca.is_none() {
                am.ca = Set(cp.ca.clone());
            }
            if let Err(e) = am.update(db).await {
                error!(hash = %o.hash, error = %e, "failed to persist upstream availability");
            }
        }

        // A derivation is substitutable iff every output is cached somewhere.
        let available: HashSet<String> = outputs
            .iter()
            .filter(|o| o.is_cached_anywhere())
            .map(|o| o.hash.clone())
            .chain(found.keys().cloned())
            .collect();

        Ok(derivations_all_outputs_available(&outputs, &available))
    }

    /// Probe `<hash>.narinfo` across `upstream_urls` for each `(hash, store_path)`
    /// at bounded concurrency, returning the resolved narinfo per found hash.
    async fn probe_upstreams(
        &self,
        upstream_urls: &[String],
        targets: Vec<(String, String)>,
    ) -> std::collections::HashMap<String, gradient_types::proto::CachedPath> {
        use futures::stream::{FuturesUnordered, StreamExt as _};
        const CONCURRENCY: usize = 16;

        let mut found = std::collections::HashMap::new();
        if targets.is_empty() {
            return found;
        }
        let urls = Arc::new(upstream_urls.to_vec());
        let http = self.state.http.clone();
        let mut futs = FuturesUnordered::new();
        let mut iter = targets.into_iter();
        let push = |futs: &mut FuturesUnordered<_>, hash: String, path: String| {
            let http = http.clone();
            let urls = Arc::clone(&urls);
            let key = hash.clone();
            futs.push(async move {
                (
                    key,
                    gradient_core::upstream::lookup_upstream_narinfo(http, urls, hash, path).await,
                )
            });
        };

        for _ in 0..CONCURRENCY {
            match iter.next() {
                Some((hash, path)) => push(&mut futs, hash, path),
                None => break,
            }
        }
        while let Some((hash, res)) = futs.next().await {
            if let Some(cp) = res {
                found.insert(hash, cp);
            }
            if let Some((hash, path)) = iter.next() {
                push(&mut futs, hash, path);
            }
        }
        found
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
                &self.state.db(),
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
                &self.state.db(),
                self.evaluation_id,
                MessageLevel::Warning,
                warning.clone(),
                Some("nix-eval".to_string()),
            )
            .await;
        }
        for error in errors {
            record_evaluation_message(
                &self.state.db(),
                self.evaluation_id,
                MessageLevel::Error,
                error.clone(),
                Some("nix-eval".to_string()),
            )
            .await;
        }
    }

    /// Insert project entry points and schedule per-project evaluation GC.
    async fn process_entry_points(
        &self,
        project_id: ProjectId,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) {
        let now = gradient_types::now();

        let mut active_entry_points: Vec<AEntryPoint> = Vec::new();

        for d in derivations {
            if d.attr.is_empty() {
                continue;
            }
            if let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) {
                active_entry_points.push(MEntryPoint {
                    id: EntryPointId::now_v7(),
                    project: project_id,
                    evaluation: self.evaluation_id,
                    derivation: drv_id,
                    eval: d.attr.clone(),
                    created_at: now,
                    ..Default::default()
                }.into_active_model());
            }
        }

        if !active_entry_points.is_empty() {
            for chunk in active_entry_points.chunks(BATCH_SIZE) {
                if let Err(e) = EEntryPoint::insert_many(chunk.to_vec())
                    .exec(&self.state.worker_db)
                    .await
                {
                    error!(error = %e, "failed to insert entry points");
                }
            }
        }

        // GC: remove old evaluations beyond keep_evaluations for this project.
        if let Ok(Some(project)) = EProject::find_by_id(project_id)
            .one(&self.state.worker_db)
            .await
        {
            let gc_state = Arc::clone(self.state);
            let gc_keep = project.keep_evaluations as usize;
            self.state.shutdown.spawn(async move {
                if let Err(e) =
                    gradient_db::gc_project_evaluations(&gc_state.db(), project_id, gc_keep).await
                {
                    error!(error = %e, %project_id, "GC: per-project evaluation GC failed");
                }
            });
        }
    }
}

// ── Public handlers ───────────────────────────────────────────────────────────

pub async fn handle_eval_result(
    state: &Arc<ServerState>,
    job: &PendingEvalJob,
    mut derivations: Vec<DiscoveredDerivation>,
    warnings: Vec<String>,
    errors: Vec<String>,
) -> Result<()> {
    // `derivation.derivation_path` stores the bare `<hash>-<name>` form
    // (narinfo `References:` convention). Callers may pass either form;
    // normalise once here so every downstream key (existing-rows map,
    // insert path, build-row lookup, deferred deps) is in canonical form.
    for d in &mut derivations {
        d.drv_path = strip_nix_store_prefix(&d.drv_path);
        for dep in &mut d.dependencies {
            *dep = strip_nix_store_prefix(dep);
        }
    }

    let evaluation_id = job.evaluation_id;

    let current = EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await
        .context("fetch evaluation")?;

    let evaluation = match current {
        Some(e) if e.status == EvaluationStatus::Aborted => {
            info!(%evaluation_id, "evaluation aborted; discarding worker result");
            return Ok(());
        }
        Some(e) => e,
        None => anyhow::bail!("evaluation {} not found", evaluation_id),
    };

    info!(
        %evaluation_id,
        derivation_count = derivations.len(),
        warning_count = warnings.len(),
        error_count = errors.len(),
        "processing eval result from worker",
    );

    let proc = EvalResultProcessor::new(state, evaluation_id, evaluation);

    let existing = proc.load_existing_derivations(&derivations).await?;
    let batch = DerivationInsertBatch::prepare(&derivations, &existing);
    let drv_path_to_id = batch.insert(state, &proc.evaluation).await?;

    // Dependency edges are NOT created here. The BFS walks roots→leaves, so
    // batch N may contain derivation A whose dep B lands in batch N+1. Edges are
    // accumulated per eval and flushed by `flush_deferred_deps` once the stream
    // completes (`handle_eval_job_completed`), when every endpoint has a row.

    proc.resolve_anchors(&derivations, &drv_path_to_id)
        .await?;

    proc.add_system_features(&derivations, &drv_path_to_id)
        .await;

    proc.record_eval_messages(&warnings, &errors).await;

    // Errors are stored as evaluation_message rows and will cause
    // check_evaluation_done to mark the evaluation as Failed once all
    // queued builds finish (or immediately if there are no builds at all).
    // Do NOT mark Failed here: a later batch or a previous batch may have
    // queued builds that should still run.

    if let Some(project_id) = job.project_id {
        proc.process_entry_points(project_id, &derivations, &drv_path_to_id)
            .await;
    }

    // Must run after `process_entry_points`: the forge reporter skips build
    // events without a matching `entry_point` row, so emitting
    // `build.substituted` earlier would drop every check.
    if let Err(e) = proc.dispatch_substituted_events().await {
        tracing::warn!(error = %e, "failed to dispatch build.substituted events");
    }

    // Builds and entry-points are inserted directly (no status transition), so
    // the live channels would otherwise stay silent for the whole evaluation
    // phase. Ping subscribers so the project/eval pages refetch and grow their
    // build totals as each batch lands.
    let _ = state.board_events.send(BoardEvent::EvaluationProgress {
        project: job.project_id.map(|p| p.into_inner()),
        evaluation_id: evaluation_id.into_inner(),
    });

    debug!(
        %evaluation_id,
        new_derivations = derivations.len(),
        "eval batch persisted; awaiting more batches"
    );
    Ok(())
}

pub async fn handle_eval_job_completed(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
) -> Result<()> {
    // The build graph is now complete: materialise each entry point's closure
    // and seed the per-entry-point dependency counts (#383).
    if let Err(e) = gradient_db::seed_entry_point_dep_counts(&state.worker_db, evaluation_id).await {
        error!(error = %e, %evaluation_id, "seed_entry_point_dep_counts failed (non-fatal)");
    }

    // The dependency graph is now complete (edges flushed). Mark this eval's
    // anchors edges_complete so promotion and dispatch may consider them: an
    // anchor stays gated until the eval that owns it flushes a full edge set, so
    // a still-running, failed, or interrupted eval can never get its 0-edge
    // anchors promoted as if they were dependency-free.
    if let Err(e) = gradient_db::mark_edges_complete_for_eval(&state.worker_db, evaluation_id).await
    {
        error!(error = %e, %evaluation_id, "mark_edges_complete_for_eval failed");
    }

    // Seed the graph-driven promotion from its ready frontier: leaves and
    // anchors whose deps were already cached/substituted. Each subsequent
    // completion cascades upward.
    if let Err(e) = gradient_db::promote_ready(&state.worker_db).await {
        error!(error = %e, %evaluation_id, "promote_ready failed");
    }

    // Promotion is graph-driven (gradient_db::promotion), independent of eval
    // completion, so finishing the stream just advances the eval to Building.
    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await?
        && matches!(
            eval.status,
            EvaluationStatus::EvaluatingFlake | EvaluationStatus::EvaluatingDerivation
        )
    {
        info!(%evaluation_id, "eval job complete; promoting evaluation to Building");
        update_evaluation_status(&state.db(), eval, EvaluationStatus::Building).await;
    }

    // If every build was already terminal (e.g. all Substituted), close the
    // evaluation out via the shared decision function.
    check_evaluation_done(state, evaluation_id).await
}

/// Resolve `(drv_path, Vec<dep_drv_path>)` pairs to `(derivation_uuid,
/// dep_uuid)` edges and insert them (conflict-do-nothing). Called once at
/// `handle_eval_job_completed` with the evaluation's accumulated edges, when
/// every endpoint derivation has a row.
pub async fn flush_deferred_deps(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    deferred: Vec<(String, Vec<String>)>,
) -> Result<()> {
    if deferred.is_empty() {
        return Ok(());
    }

    // Collect every unique drv_path mentioned (both as source and as dep), and
    // derive the unique hash set we'll filter the DB on. Hashes are
    // content-addressed (32-char nix32) so filtering by hash alone is enough to
    // pin a row down in the global derivation graph.
    let mut all_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
    for (src, deps) in &deferred {
        all_paths.insert(src.clone());
        for d in deps {
            all_paths.insert(d.clone());
        }
    }
    let all_hashes: Vec<String> = all_paths
        .iter()
        .filter_map(|p| parse_drv_hash_name(p).ok().map(|(h, _)| h))
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    let db = &state.worker_db;
    let drv_path_to_id: std::collections::HashMap<String, DerivationId> =
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

    let mut edges: Vec<ADerivationDependency> = Vec::new();
    let mut unresolved = 0usize;
    for (src, deps) in &deferred {
        let Some(&src_id) = drv_path_to_id.get(src) else {
            unresolved += 1;
            continue;
        };
        for dep in deps {
            if let Some(&dep_id) = drv_path_to_id.get(dep) {
                edges.push(MDerivationDependency {
                    id: DerivationDependencyId::now_v7(),
                    derivation: src_id,
                    dependency: dep_id,
                }.into_active_model());
            } else {
                unresolved += 1;
            }
        }
    }

    if !edges.is_empty() {
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
                .do_nothing()
                .exec(&state.worker_db)
                .await
            {
                error!(error = %e, "flush_deferred_deps: failed to insert edges");
            }
        }
    }

    info!(
        %evaluation_id,
        inserted = edges.len(),
        unresolved,
        "flushed deferred dependency edges"
    );
    Ok(())
}

pub async fn handle_eval_job_failed(
    state: &Arc<ServerState>,
    evaluation_id: EvaluationId,
    error: &str,
) -> Result<()> {
    if let Some(eval) = EEvaluation::find_by_id(evaluation_id)
        .one(&state.worker_db)
        .await?
        && !matches!(
            eval.status,
            EvaluationStatus::Completed | EvaluationStatus::Failed | EvaluationStatus::Aborted
        )
    {
        update_evaluation_status_with_error(
            &state.db(),
            eval,
            EvaluationStatus::Failed,
            error.to_owned(),
            Some("worker".to_string()),
        )
        .await;
    }
    Ok(())
}

/// Derivations whose *every* output hash is in `available` (cached in the
/// gradient cache or resolved at an org upstream). All-or-nothing: a derivation
/// is substitutable only when none of its outputs would still have to be built.
fn derivations_all_outputs_available(
    outputs: &[MDerivationOutput],
    available: &std::collections::HashSet<String>,
) -> std::collections::HashSet<DerivationId> {
    let mut by_drv: HashMap<DerivationId, Vec<&MDerivationOutput>> = HashMap::new();
    for o in outputs {
        by_drv.entry(o.derivation).or_default().push(o);
    }

    by_drv
        .into_iter()
        .filter(|(_, outs)| !outs.is_empty() && outs.iter().all(|o| available.contains(&o.hash)))
        .map(|(drv_id, _)| drv_id)
        .collect()
}

#[cfg(test)]
mod upstream_substitutable_tests {
    use super::*;
    use std::collections::HashSet;

    fn output(drv: DerivationId, hash: &str) -> MDerivationOutput {
        MDerivationOutput {
            derivation: drv,
            hash: hash.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn substitutable_only_when_all_outputs_available() {
        let a = DerivationId::now_v7();
        let b = DerivationId::now_v7();
        let outputs = vec![
            output(a, "h1"),
            output(a, "h2"),
            output(b, "h3"),
            output(b, "h4"),
        ];
        let available: HashSet<String> =
            ["h1", "h2", "h3"].iter().map(|s| s.to_string()).collect();

        let got = derivations_all_outputs_available(&outputs, &available);
        assert!(got.contains(&a), "all of a's outputs are available");
        assert!(!got.contains(&b), "b has an output (h4) not cached anywhere");
    }

    #[test]
    fn no_outputs_is_not_substitutable() {
        assert!(derivations_all_outputs_available(&[], &HashSet::new()).is_empty());
    }
}
