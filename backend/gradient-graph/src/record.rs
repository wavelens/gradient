/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::events::evaluation;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow};
use gradient_db::{
    DbContext, MissingRow, WorkerDb,
    status::{EvaluationFailure, report_worker_message, update_evaluation_status_with_error},
};
use gradient_entity::StorePath;
use gradient_entity::build::BuildStatus;
use gradient_entity::entry_point::{self, Licenses};
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_types::runner::{runner_image_attr, runner_refusal};
use gradient_types::*;
use gradient_wire::types::{DiscoveredDerivation, License, attr_eval_source};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel,
    QueryFilter, Value,
};
use tracing::{debug, error, warn};

use crate::messages::{RecordBatch, RecordReport, UpstreamHit};

const BATCH_SIZE: usize = 1000;

gradient_db::sql! {
    WALKED_UPSERT = r#"
INSERT INTO derivation
    (id, hash, name, architecture, pname, prefer_local_build, is_fixed_output, allow_substitutes, walked, unwalked_inputs, created_at)
SELECT d.id, d.hash, d.name, d.architecture, NULLIF(d.pname, ''), d.prefer_local_build,
       d.is_fixed_output, d.allow_substitutes, true, d.unwalked_inputs, $10
FROM unnest($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::bool[], $7::bool[], $8::bool[], $9::int[])
     AS d(id, hash, name, architecture, pname, prefer_local_build, is_fixed_output, allow_substitutes, unwalked_inputs)
ON CONFLICT (hash, name) DO UPDATE SET
    architecture = EXCLUDED.architecture,
    pname = EXCLUDED.pname,
    prefer_local_build = EXCLUDED.prefer_local_build,
    is_fixed_output = EXCLUDED.is_fixed_output,
    allow_substitutes = EXCLUDED.allow_substitutes,
    walked = true,
    unwalked_inputs = EXCLUDED.unwalked_inputs
WHERE NOT derivation.walked
RETURNING hash
"#,
        params = [
            NewUuids(64), DerivationHashes(64), Texts("hello", 64), Texts("x86_64-linux", 64),
            Texts("hello-1.0", 64), Bools(false, 64), Bools(false, 64), Bools(true, 64), Ints(0, 64),
            Now,
        ];

    STUB_INSERT = r#"
INSERT INTO derivation (id, hash, name, architecture, walked, created_at)
SELECT d.id, d.hash, d.name, '', false, $4
FROM unnest($1::uuid[], $2::text[], $3::text[]) AS d(id, hash, name)
ON CONFLICT (hash, name) DO NOTHING
"#,
        params = [NewUuids(64), DerivationHashes(64), Texts("hello", 64), Now];

    RESOLVE_IDS = "SELECT id, hash FROM derivation WHERE hash = ANY($1::text[])",
        params = [DerivationHashes(64)];

    EDGE_INSERT = r#"
INSERT INTO derivation_dependency (derivation, dependency)
SELECT DISTINCT e.derivation, e.dependency FROM unnest($1::uuid[], $2::uuid[]) AS e(derivation, dependency)
ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 1
RETURNING derivation
"#,
        params = [DerivationIds(64), DerivationIds(64)];

    SHARED_BUILD_LIMITS_UPDATE = r#"
UPDATE derivation_build AS db
SET timeout_secs = NULLIF(l.timeout_secs, 0), max_silent_secs = NULLIF(l.max_silent_secs, 0)
FROM unnest($1::uuid[], $2::bigint[], $3::bigint[]) AS l(derivation, timeout_secs, max_silent_secs)
WHERE db.derivation = l.derivation
  AND (db.timeout_secs, db.max_silent_secs)
      IS DISTINCT FROM (NULLIF(l.timeout_secs, 0), NULLIF(l.max_silent_secs, 0))
"#,
        params = [DerivationIds(64), Ints(3600, 64), Ints(600, 64)];

    MARK_PROBED = r#"
UPDATE derivation_build db
SET probed = true, probed_at = (now() AT TIME ZONE 'UTC'), updated_at = (now() AT TIME ZONE 'UTC')
WHERE db.derivation = ANY($1::uuid[]) AND NOT db.probed
  AND EXISTS (SELECT 1 FROM derivation w WHERE w.id = db.derivation AND w.walked)
  AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.derivation = db.derivation)
RETURNING db.derivation
"#,
        params = [DerivationIds(64)];

    MARK_IFD = "UPDATE derivation SET ifd = true WHERE hash = ANY($1::text[]) AND NOT ifd",
        params = [DerivationHashes(64)];
}

gradient_db::sql_fn! {
    FLIP_CACHE_AVAILABLE = flip_cache_available_sql,
        params = [DerivationIds(64)];
}

fn flip_cache_available_sql() -> String {
    format!(
        "UPDATE derivation_build SET cache_available = true, \
         updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE derivation = ANY($1::uuid[]) AND NOT cache_available \
           AND status NOT IN ({terminal_success}) \
         RETURNING derivation",
        terminal_success = gradient_db::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
    )
}

struct Resolved {
    by_path: HashMap<String, DerivationId>,
    by_hash: HashMap<String, DerivationId>,
}

struct BatchWriter<'a> {
    ctx: &'a DbContext,
    evaluation_id: EvaluationId,
}

impl BatchWriter<'_> {
    fn db(&self) -> &WorkerDb {
        &self.ctx.worker_db
    }

    fn txn(&self) -> Result<&sea_orm::DatabaseTransaction> {
        self.db()
            .as_transaction()
            .context("a batch is written inside the graph writer's transaction")
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn upsert_walked(&self, derivations: &[DiscoveredDerivation]) -> Result<HashSet<String>> {
        let mut seen = HashSet::new();
        let mut ids: Vec<uuid::Uuid> = Vec::new();
        let mut hashes: Vec<String> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        let mut architectures: Vec<String> = Vec::new();
        let mut pnames: Vec<String> = Vec::new();
        let mut prefer_local: Vec<bool> = Vec::new();
        let mut fixed_output: Vec<bool> = Vec::new();
        let mut allow_substitutes: Vec<bool> = Vec::new();
        let mut input_counts: Vec<i32> = Vec::new();
        for d in derivations {
            let (hash, name) = drv_hash_name(&d.drv_path).ok_or_else(|| {
                anyhow!(
                    "reported derivation is not a derivation path: {}",
                    d.drv_path
                )
            })?;
            if !seen.insert(hash.clone()) {
                continue;
            }

            ids.push(DerivationId::now_v7().into_inner());
            hashes.push(hash);
            names.push(name);
            architectures.push(d.architecture.clone());
            pnames.push(d.pname.clone().unwrap_or_default());
            prefer_local.push(d.prefer_local_build);
            fixed_output.push(d.is_fixed_output);
            allow_substitutes.push(d.allow_substitutes);
            input_counts.push(
                i32::try_from(d.dependencies.iter().collect::<HashSet<_>>().len())
                    .unwrap_or(i32::MAX),
            );
        }

        if hashes.is_empty() {
            return Ok(HashSet::new());
        }

        let rows = self
            .db()
            .query_all_raw(WALKED_UPSERT.bind([
                ids.into(),
                hashes.into(),
                names.into(),
                architectures.into(),
                pnames.into(),
                prefer_local.into(),
                fixed_output.into(),
                allow_substitutes.into(),
                input_counts.into(),
                Value::ChronoDateTime(Some(gradient_types::now())),
            ]))
            .await
            .context("upsert walked derivations")?;

        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<String>("", "hash").ok())
            .collect())
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn mark_imported(&self, derivations: &[DiscoveredDerivation]) -> Result<()> {
        let hashes: Vec<String> = derivations
            .iter()
            .filter(|d| d.ifd)
            .filter_map(|d| drv_hash_name(&d.drv_path).map(|(hash, _)| hash))
            .collect();
        if hashes.is_empty() {
            return Ok(());
        }

        self.db()
            .execute_raw(MARK_IFD.bind([hashes.into()]))
            .await
            .context("mark imported derivations")?;

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn insert_stubs(&self, derivations: &[DiscoveredDerivation]) -> Result<()> {
        let walked: HashSet<&str> = derivations.iter().map(|d| d.drv_path.as_str()).collect();
        let mut seen = HashSet::new();
        let mut ids: Vec<uuid::Uuid> = Vec::new();
        let mut hashes: Vec<String> = Vec::new();
        let mut names: Vec<String> = Vec::new();
        for d in derivations {
            for dep in &d.dependencies {
                if walked.contains(dep.as_str()) {
                    continue;
                }

                let (hash, name) = drv_hash_name(dep).ok_or_else(|| {
                    anyhow!(
                        "derivation {} depends on {dep}, which is not a derivation path",
                        d.drv_path
                    )
                })?;
                if !seen.insert(hash.clone()) {
                    continue;
                }

                ids.push(DerivationId::now_v7().into_inner());
                hashes.push(hash);
                names.push(name);
            }
        }

        if hashes.is_empty() {
            return Ok(());
        }

        self.db()
            .execute_raw(STUB_INSERT.bind([
                ids.into(),
                hashes.into(),
                names.into(),
                Value::ChronoDateTime(Some(gradient_types::now())),
            ]))
            .await
            .context("insert dependency stubs")?;

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn resolve_ids(&self, derivations: &[DiscoveredDerivation]) -> Result<Resolved> {
        let paths: HashSet<&str> = derivations
            .iter()
            .flat_map(|d| {
                std::iter::once(d.drv_path.as_str())
                    .chain(d.dependencies.iter().map(String::as_str))
            })
            .collect();
        let hashes: Vec<String> = paths
            .iter()
            .filter_map(|p| drv_hash_name(p).map(|(h, _)| h))
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let mut by_hash: HashMap<String, DerivationId> = HashMap::new();
        for chunk in hashes.chunks(gradient_db::IN_CHUNK_SIZE) {
            let rows = self
                .db()
                .query_all_raw(RESOLVE_IDS.bind([chunk.to_vec().into()]))
                .await
                .context("resolve derivation ids")?;
            for r in rows {
                if let (Ok(id), Ok(hash)) = (
                    r.try_get::<uuid::Uuid>("", "id"),
                    r.try_get::<String>("", "hash"),
                ) {
                    by_hash.insert(hash, DerivationId::new(id));
                }
            }
        }

        let by_path = paths
            .into_iter()
            .filter_map(|p| {
                let (hash, _) = drv_hash_name(p)?;
                by_hash.get(&hash).map(|id| (p.to_owned(), *id))
            })
            .collect();

        Ok(Resolved { by_path, by_hash })
    }

    /// Re-asserting the full declared set on every walk is the only repair for a lost edge.
    /// Both inserts are conflict-guarded no-ops on a record already written.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn insert_records(
        &self,
        derivations: &[DiscoveredDerivation],
        ids: &HashMap<String, DerivationId>,
    ) -> Result<Vec<DerivationId>> {
        let now = gradient_types::now();
        let mut outputs: Vec<ADerivationOutput> = Vec::new();
        let mut edge_from: Vec<uuid::Uuid> = Vec::new();
        let mut edge_to: Vec<uuid::Uuid> = Vec::new();
        for d in derivations {
            let Some(&id) = ids.get(&d.drv_path) else {
                continue;
            };
            for output in &d.outputs {
                let (hash, package) = output_hash_name(&output.path).unzip();
                outputs.push(
                    MDerivationOutput {
                        id: DerivationOutputId::now_v7(),
                        derivation: id,
                        name: output.name.clone(),
                        hash,
                        package,
                        path_known_after_build: output.path.is_empty(),
                        created_at: now,
                        ..Default::default()
                    }
                    .into_active_model(),
                );
            }

            for dep in &d.dependencies {
                if let Some(&dep_id) = ids.get(dep) {
                    edge_from.push(id.into_inner());
                    edge_to.push(dep_id.into_inner());
                }
            }
        }

        for chunk in outputs.chunks(BATCH_SIZE) {
            let res = EDerivationOutput::insert_many(chunk.to_vec())
                .on_conflict(
                    sea_orm::sea_query::OnConflict::columns([
                        CDerivationOutput::Derivation,
                        CDerivationOutput::Name,
                    ])
                    .do_nothing()
                    .to_owned(),
                )
                .exec(self.db())
                .await;
            if let Err(e) = res
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                return Err(anyhow::Error::new(e).context("insert derivation outputs"));
            }
        }

        if edge_from.is_empty() {
            return Ok(Vec::new());
        }

        let grew = self
            .db()
            .query_all_raw(EDGE_INSERT.bind([edge_from.into(), edge_to.into()]))
            .await
            .context("insert dependency edges")?;

        let mut grown: Vec<DerivationId> = grew
            .iter()
            .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
            .map(DerivationId::new)
            .collect();
        grown.sort_unstable();
        grown.dedup();

        Ok(grown)
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn record_walk_completeness(
        &self,
        resolved: &Resolved,
        newly_walked: &HashSet<String>,
        grew: &[DerivationId],
    ) -> Result<()> {
        let walked: Vec<DerivationId> = newly_walked
            .iter()
            .filter_map(|h| resolved.by_hash.get(h).copied())
            .collect();
        if walked.is_empty() && grew.is_empty() {
            return Ok(());
        }

        gradient_db::graph::walk_completeness::seed_walk_completeness(self.txn()?, &walked, grew)
            .await
            .context("seed unwalked_inputs")?;

        Ok(())
    }

    async fn set_shared_build_limits(
        &self,
        limits: &HashMap<DerivationId, (Option<i64>, Option<i64>)>,
    ) -> Result<()> {
        let mut ids: Vec<uuid::Uuid> = Vec::new();
        let mut timeouts: Vec<i64> = Vec::new();
        let mut silents: Vec<i64> = Vec::new();
        for (id, (timeout_secs, max_silent_secs)) in limits {
            if timeout_secs.is_none() && max_silent_secs.is_none() {
                continue;
            }
            ids.push(id.into_inner());
            timeouts.push(timeout_secs.unwrap_or(0));
            silents.push(max_silent_secs.unwrap_or(0));
        }
        if ids.is_empty() {
            return Ok(());
        }

        self.db()
            .execute_raw(SHARED_BUILD_LIMITS_UPDATE.bind([
                ids.into(),
                timeouts.into(),
                silents.into(),
            ]))
            .await
            .context("set shared build limits")?;

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn stored_derivations(
        &self,
        derivations: &[DiscoveredDerivation],
        ids: &HashMap<String, DerivationId>,
    ) -> Result<HashSet<DerivationId>> {
        let outputs_by_drv: Vec<(DerivationId, Vec<String>)> = derivations
            .iter()
            .filter_map(|d| {
                let hashes: Vec<String> = d
                    .outputs
                    .iter()
                    .filter_map(|o| StorePath::parse_stored(&o.path).ok())
                    .map(|sp| sp.hash().to_owned())
                    .collect();
                ids.get(&d.drv_path)
                    .filter(|_| !hashes.is_empty())
                    .map(|id| (*id, hashes))
            })
            .collect();
        let all_hashes: Vec<String> = outputs_by_drv
            .iter()
            .flat_map(|(_, hashes)| hashes.iter().cloned())
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();

        let db = self.db();
        let complete: HashSet<String> =
            gradient_db::fetch_in_chunks(&all_hashes, |chunk| async move {
                gradient_db::graph::runtime_can_start::complete_output_hashes(db, &chunk).await
            })
            .await
            .context("look up the batch's complete outputs")?
            .into_iter()
            .collect();

        Ok(outputs_by_drv
            .into_iter()
            .filter(|(_, hashes)| hashes.iter().all(|h| complete.contains(h)))
            .map(|(id, _)| id)
            .collect())
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn resolve_shared_builds(
        &self,
        ids: &HashMap<String, DerivationId>,
        derivations: &[DiscoveredDerivation],
        stored: &HashSet<DerivationId>,
    ) -> Result<()> {
        let now = gradient_types::now();
        let all_ids: Vec<DerivationId> = ids
            .values()
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let limits: HashMap<DerivationId, (Option<i64>, Option<i64>)> = derivations
            .iter()
            .filter_map(|d| {
                ids.get(&d.drv_path).map(|id| {
                    (
                        *id,
                        (
                            d.timeout_secs.map(|v| v as i64),
                            d.max_silent_secs.map(|v| v as i64),
                        ),
                    )
                })
            })
            .collect();

        let shared_builds: Vec<ADerivationBuild> = all_ids
            .iter()
            .map(|&drv_id| {
                let status = if stored.contains(&drv_id) {
                    BuildStatus::Substituted
                } else {
                    BuildStatus::Created
                };
                let (timeout_secs, max_silent_secs) =
                    limits.get(&drv_id).copied().unwrap_or((None, None));

                MDerivationBuild {
                    id: DerivationBuildId::now_v7(),
                    derivation: drv_id,
                    status,
                    cache_available: false,
                    probed: false,
                    substituted: status == BuildStatus::Substituted,
                    wanted: false,
                    timeout_secs,
                    max_silent_secs,
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                }
                .into_active_model()
            })
            .collect();

        for chunk in shared_builds.chunks(BATCH_SIZE) {
            let res = EDerivationBuild::insert_many(chunk.to_vec())
                .on_conflict(
                    sea_orm::sea_query::OnConflict::column(CDerivationBuild::Derivation)
                        .do_nothing()
                        .to_owned(),
                )
                .exec(self.db())
                .await;
            if let Err(e) = res
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                return Err(anyhow::Error::new(e).context("upsert shared builds"));
            }
        }
        self.set_shared_build_limits(&limits).await?;

        let db = self.db();
        let shared_build_by_drv: HashMap<DerivationId, DerivationBuildId> =
            gradient_db::fetch_in_chunks(&all_ids, |chunk| async move {
                EDerivationBuild::find()
                    .filter(CDerivationBuild::Derivation.is_in(chunk))
                    .all(db)
                    .await
            })
            .await?
            .into_iter()
            .map(|a| (a.derivation, a.id))
            .collect();

        let jobs: Vec<ABuildJob> = all_ids
            .iter()
            .filter_map(|drv_id| {
                shared_build_by_drv.get(drv_id).map(|&shared_build_id| {
                    MBuildJob {
                        id: gradient_types::ids::BuildJobId::now_v7(),
                        evaluation: self.evaluation_id,
                        derivation: *drv_id,
                        derivation_build: shared_build_id,
                        score_breakdown: serde_json::json!({}),
                        created_at: now,
                        ..Default::default()
                    }
                    .into_active_model()
                })
            })
            .collect();

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
                .exec(db)
                .await;
            if let Err(e) = res
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                return Err(anyhow::Error::new(e).context("upsert build_job rows"));
            }
        }

        if !stored.is_empty() {
            let stored_ids: Vec<DerivationId> = stored.iter().copied().collect();
            let changes =
                gradient_db::graph::promotion::substitute_created_shared_builds(db, &stored_ids)
                    .await
                    .context("substitute created shared builds")?;
            gradient_db::status::emit_transition_effects(self.ctx, &changes).await?;
        }

        Ok(())
    }

    /// One lock is covering the union of both sets to avoid an ABBA cycle with a retire.
    /// The un-promote is covering a row the stale-low ripple queued and the seed raised again.
    /// Only the net transition committed, and only the net transition may fan out.
    #[tracing::instrument(level = "debug", skip_all)]
    async fn advance_can_start(
        &self,
        stored: &HashSet<DerivationId>,
        resolved: &Resolved,
        newly_walked: &HashSet<String>,
        grew: &[DerivationId],
        adopted: &[DerivationId],
        entry_points: &[DerivationId],
    ) -> Result<Vec<DerivationId>> {
        let mut to_seed: Vec<DerivationId> = newly_walked
            .iter()
            .filter_map(|h| resolved.by_hash.get(h).copied())
            .collect();
        to_seed.extend_from_slice(grew);
        to_seed.extend_from_slice(adopted);
        to_seed.sort_unstable();
        to_seed.dedup();

        let mut locked = to_seed.clone();
        locked.extend(stored.iter().copied());
        locked.sort_unstable();
        locked.dedup();
        if locked.is_empty() {
            return self.move_batch_need(&to_seed, entry_points).await;
        }

        let txn = self.txn()?;
        let lock = gradient_db::graph::can_start::lock_seed_shared_builds(txn, &locked).await?;
        let mut changes = gradient_db::graph::can_start::became_fetchable(&lock)
            .await
            .context("advance fetchable shared builds")?;
        gradient_db::graph::can_start::seed_blocking_deps(&lock)
            .await
            .context("seed blocking_deps")?;
        changes.extend(
            gradient_db::graph::can_start::promote(txn, &locked)
                .await
                .context("promote the batch's shared builds")?,
        );
        changes.extend(
            gradient_db::graph::can_start::unpromote_ungated(txn, &locked)
                .await
                .context("settle the queue against the seeded counts")?,
        );
        let net = gradient_db::status::collapse_transitions(changes);
        gradient_db::status::emit_transition_effects(self.ctx, &net).await?;
        self.move_batch_need(&to_seed, entry_points).await
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn adopt_references(
        &self,
        resolved: &Resolved,
        newly_walked: &HashSet<String>,
    ) -> Result<Vec<DerivationId>> {
        let walked: Vec<DerivationId> = newly_walked
            .iter()
            .filter_map(|h| resolved.by_hash.get(h).copied())
            .collect();
        if walked.is_empty() {
            return Ok(Vec::new());
        }

        let txn = self.txn()?;
        let wanted_by =
            gradient_db::graph::runtime_dependencies::adopt_referenced_outputs(txn, &walked)
                .await
                .context("adopt the runtime references recorded before either end was walked")?;
        if wanted_by.is_empty() {
            return Ok(wanted_by);
        }

        debug!(
            wanted_by = wanted_by.len(),
            "adopted runtime dependencies recorded before either end was walked"
        );
        let mut changes = gradient_db::graph::can_start::update_and_settle_need(txn, &wanted_by)
            .await?
            .changes;
        let seeded =
            gradient_db::graph::runtime_can_start::seed_runtime_deps(txn, &[], &wanted_by).await?;
        if !seeded.incomplete.is_empty() {
            let lock =
                gradient_db::graph::can_start::lock_shared_builds(txn, &seeded.incomplete).await?;
            changes.extend(gradient_db::graph::can_start::lost_fetchability(&lock).await?);
        }
        gradient_db::status::emit_transition_effects(self.ctx, &changes).await?;

        Ok(wanted_by)
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn move_batch_need(
        &self,
        builders: &[DerivationId],
        entry_points: &[DerivationId],
    ) -> Result<Vec<DerivationId>> {
        let db = self.db();
        let mut roots = builders.to_vec();
        roots.extend_from_slice(entry_points);
        roots.sort_unstable();
        roots.dedup();

        let mut changes = Vec::new();
        let mut gained_need = Vec::new();
        for chunk in roots.chunks(gradient_db::IN_CHUNK_SIZE) {
            let settled = gradient_db::graph::can_start::update_and_settle_need(db, chunk)
                .await
                .context("settle what this batch needs built")?;
            gained_need.extend_from_slice(&settled.moved.gained);
            changes.extend(settled.changes);
        }
        gradient_db::status::emit_transition_effects(self.ctx, &changes).await?;

        Ok(gained_need)
    }

    async fn settle_created(&self, ids: &HashMap<String, DerivationId>) -> Result<()> {
        let mut derivations: Vec<DerivationId> = ids.values().copied().collect();
        derivations.sort_unstable();
        derivations.dedup();
        let changes = gradient_db::graph::can_start::settle_created(self.db(), &derivations)
            .await
            .context("settle the shared builds this batch left created")?;
        gradient_db::status::emit_transition_effects(self.ctx, &changes).await?;

        Ok(())
    }

    #[tracing::instrument(level = "debug", skip_all)]
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
                let Ok(store_path) = StorePath::parse_stored(src) else {
                    continue;
                };

                let hash = store_path.hash().to_owned();
                if !seen.insert((drv_id, hash.clone())) {
                    continue;
                }

                rows.push(
                    MDerivationInputSource {
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

    #[tracing::instrument(level = "debug", skip_all)]
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

            if let Err(e) = gradient_db::graph::features::add_features(
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

    #[tracing::instrument(level = "debug", skip_all)]
    async fn claim_cached_outputs(&self, ids: &HashMap<String, DerivationId>) -> Result<()> {
        let mut jobs: Vec<DerivationId> = ids.values().copied().collect();
        jobs.sort_unstable();
        jobs.dedup();

        crate::claims::claim_reused_outputs(self.ctx, self.evaluation_id, &jobs)
            .await
            .context("claim the cached outputs of the batch's jobs")
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn record_eval_messages(&self, warnings: &[String], errors: &[String]) {
        for warning in warnings {
            report_worker_message(
                self.ctx,
                self.evaluation_id,
                MessageLevel::Warning,
                warning.clone(),
                "nix-eval".to_owned(),
            )
            .await;
        }

        for error in errors {
            report_worker_message(
                self.ctx,
                self.evaluation_id,
                MessageLevel::Error,
                error.clone(),
                "nix-eval".to_owned(),
            )
            .await;
        }
    }

    #[tracing::instrument(level = "debug", skip_all)]
    async fn process_entry_points(
        &self,
        task_id: TaskId,
        derivations: &[DiscoveredDerivation],
        drv_path_to_id: &HashMap<String, DerivationId>,
    ) -> Result<(Vec<DerivationId>, usize)> {
        let now = gradient_types::now();
        let refused = self.refused_runner_attrs(task_id, derivations).await?;
        let mut runners = 0;

        let mut active_entry_points: Vec<AEntryPoint> = Vec::new();
        let mut entry_point_drvs: Vec<DerivationId> = Vec::new();

        for d in derivations {
            if d.attr.is_empty() || refused.contains(&d.attr) {
                continue;
            }

            if let Some(&drv_id) = drv_path_to_id.get(&d.drv_path) {
                entry_point_drvs.push(drv_id);
                runners += usize::from(d.runner.is_some());
                active_entry_points.push(
                    MEntryPoint {
                        id: EntryPointId::now_v7(),
                        task: task_id,
                        evaluation: self.evaluation_id,
                        derivation: drv_id,
                        eval: d.attr.clone(),
                        licenses: entry_point_licenses(&d.licenses),
                        runner: entry_point_runner(d),
                        created_at: now,
                        ..Default::default()
                    }
                    .into_active_model(),
                );
            }
        }

        for chunk in active_entry_points.chunks(BATCH_SIZE) {
            if let Err(e) = insert_entry_points(chunk.to_vec())
                .exec(&self.ctx.worker_db)
                .await
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                error!(error = %e, "failed to insert entry points");
            }
        }

        Ok((entry_point_drvs, runners))
    }

    async fn refused_runner_attrs(
        &self,
        task_id: TaskId,
        derivations: &[DiscoveredDerivation],
    ) -> Result<HashSet<String>> {
        let image_suffix = runner_image_attr("");
        let is_image = |d: &DiscoveredDerivation| d.attr.ends_with(&image_suffix);
        if !derivations
            .iter()
            .any(|d| d.runner.is_some() || is_image(d))
        {
            return Ok(HashSet::new());
        }

        let instance_enabled = self.ctx.config.runner.enable;
        let project_enabled = self.project_runs_runner_jobs(task_id).await?;
        let mut refused = HashSet::new();
        for d in derivations {
            if let Some(spec) = &d.runner {
                if let Some(message) = runner_refusal(instance_enabled, project_enabled, spec.nodes)
                {
                    self.refuse_runner(&d.attr, message).await;
                    refused.extend([d.attr.clone(), runner_image_attr(&d.attr)]);
                }
            } else if is_image(d) && runner_refusal(instance_enabled, project_enabled, 1).is_some()
            {
                refused.insert(d.attr.clone());
            }
        }

        Ok(refused)
    }

    async fn project_runs_runner_jobs(&self, task_id: TaskId) -> Result<bool> {
        let task = ETask::find_by_id(task_id)
            .one(self.db())
            .await
            .context("fetch the task of a runner entry point")?
            .with_context(|| format!("task {task_id} not found"))?;
        let project = EProject::find_by_id(task.project)
            .one(self.db())
            .await
            .context("fetch the project of a runner entry point")?
            .with_context(|| format!("project {} not found", task.project))?;
        Ok(project.runner_enabled)
    }

    async fn refuse_runner(&self, attr: &str, message: &str) {
        report_worker_message(
            self.ctx,
            self.evaluation_id,
            MessageLevel::Error,
            message.to_owned(),
            attr_eval_source(attr),
        )
        .await;
    }
}

fn entry_point_runner(d: &DiscoveredDerivation) -> Option<sea_orm::JsonValue> {
    d.runner
        .as_ref()
        .and_then(|spec| serde_json::to_value(spec).ok())
}

fn entry_point_licenses(licenses: &[License]) -> Licenses {
    Licenses(
        licenses
            .iter()
            .map(|license| match license {
                License::Spdx { id } => entry_point::License::Spdx { id: id.clone() },
                License::Name { name } => entry_point::License::Name { name: name.clone() },
            })
            .collect(),
    )
}

fn insert_entry_points(rows: Vec<AEntryPoint>) -> sea_orm::InsertMany<AEntryPoint> {
    EEntryPoint::insert_many(rows).on_conflict(
        sea_orm::sea_query::OnConflict::columns([CEntryPoint::Evaluation, CEntryPoint::Eval])
            .do_nothing()
            .to_owned(),
    )
}

fn accepts_batches(status: EvaluationStatus, held: bool) -> bool {
    match status {
        EvaluationStatus::Fetching
        | EvaluationStatus::EvaluatingFlake
        | EvaluationStatus::EvaluatingDerivation => true,
        EvaluationStatus::Building | EvaluationStatus::Waiting => held,
        _ => false,
    }
}

#[tracing::instrument(level = "debug", skip_all, fields(eval_id = %batch.evaluation, derivations = batch.derivations.len()))]
pub(crate) async fn apply_batch(ctx: &DbContext, batch: &RecordBatch) -> Result<RecordReport> {
    let evaluation_id = batch.evaluation;
    let evaluation = EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await
        .context("fetch evaluation")?;
    let held = ctx.memory.held_evaluations.holds(evaluation_id).await;
    match evaluation {
        Some(e) if !accepts_batches(e.status, held) => {
            warn!(%evaluation_id, status = ?e.status, "batch for an evaluation that is not streaming; dropped as stale");
            return Ok(RecordReport {
                evaluation: evaluation_id,
                task: batch.task,
                skipped: true,
                ..Default::default()
            });
        }
        Some(_) => {}
        None => anyhow::bail!(MissingRow::Evaluation { id: evaluation_id }),
    }

    let writer = BatchWriter { ctx, evaluation_id };
    let mut report = RecordReport {
        evaluation: evaluation_id,
        task: batch.task,
        ..Default::default()
    };
    if !batch.derivations.is_empty() {
        let newly_walked = writer.upsert_walked(&batch.derivations).await?;
        writer.mark_imported(&batch.derivations).await?;
        writer.insert_stubs(&batch.derivations).await?;
        let resolved = writer.resolve_ids(&batch.derivations).await?;
        let ids = &resolved.by_path;
        let grew = writer.insert_records(&batch.derivations, ids).await?;
        writer
            .record_walk_completeness(&resolved, &newly_walked, &grew)
            .await?;
        writer.persist_input_sources(&batch.derivations, ids).await;
        let stored = writer.stored_derivations(&batch.derivations, ids).await?;
        writer
            .resolve_shared_builds(ids, &batch.derivations, &stored)
            .await?;
        writer.add_system_features(&batch.derivations, ids).await;
        let adopted = writer.adopt_references(&resolved, &newly_walked).await?;
        (report.entry_points, report.runner_entry_points) = match batch.task {
            Some(task) => {
                writer
                    .process_entry_points(task, &batch.derivations, ids)
                    .await?
            }
            None => (Vec::new(), 0),
        };
        report.to_probe = writer
            .advance_can_start(
                &stored,
                &resolved,
                &newly_walked,
                &grew,
                &adopted,
                &report.entry_points,
            )
            .await?;
        writer.settle_created(ids).await?;
        report.to_probe.extend(
            newly_walked
                .iter()
                .filter_map(|h| resolved.by_hash.get(h).copied()),
        );

        gradient_db::task_board::dep_counts::bump_graph_version(writer.db(), &[evaluation_id])
            .await
            .context("bump the graph version for the batch")?;

        if !grew.is_empty() {
            gradient_db::task_board::dep_counts::bump_graph_version_for_derivations(
                writer.db(),
                &grew,
            )
            .await
            .context("bump the graph version of evaluations sharing the new edges")?;
        }

        writer.claim_cached_outputs(ids).await?;
        report.walked = newly_walked.len();
        debug!(%evaluation_id, walked = report.walked, named = ids.len(), "batch written");
    }

    writer
        .record_eval_messages(&batch.warnings, &batch.errors)
        .await;
    Ok(report)
}

async fn flip_cache_available<C: ConnectionTrait>(
    db: &C,
    upstream: &[DerivationId],
) -> Result<Vec<DerivationId>> {
    if upstream.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<uuid::Uuid> = upstream.iter().map(|d| d.into_inner()).collect();
    let rows = db
        .query_all_raw(FLIP_CACHE_AVAILABLE.bind([ids.into()]))
        .await
        .context("flag shared builds cache_available from upstream")?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect())
}

pub(crate) async fn apply_upstream_hits(
    ctx: &DbContext,
    hits: &HashMap<String, UpstreamHit>,
) -> Result<Vec<DerivationId>> {
    if hits.is_empty() {
        return Ok(Vec::new());
    }

    let db = &ctx.worker_db;
    let txn = db.as_transaction().context(
        "UpstreamHits must run inside a transaction: the complete-closure seed takes shared build locks",
    )?;

    let stored =
        gradient_db::cached_paths::stored_by_hash(db, &hits.keys().cloned().collect::<Vec<_>>())
            .await
            .context("find the hit paths our cache already stores")?;
    let hashes: Vec<String> = hits
        .keys()
        .filter(|hash| !stored.contains_key(*hash))
        .cloned()
        .collect();
    let hit_rows = gradient_db::fetch_in_chunks(&hashes, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Hash.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load the outputs an upstream hit names")?;

    let mut touched: Vec<DerivationId> = hit_rows.iter().map(|o| o.derivation).collect();
    touched.sort_unstable();
    touched.dedup();
    if touched.is_empty() {
        return Ok(Vec::new());
    }

    persist_narinfo(db, &hit_rows, hits).await?;

    let outputs = gradient_db::fetch_in_chunks(&touched, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load every output of the shared builds an upstream hit names")?;
    let cached = gradient_db::derivation_outputs::cached_anywhere(db, &outputs)
        .await
        .context("find the outputs some cache already holds")?;
    let mut by_shared_build: HashMap<DerivationId, Vec<bool>> = HashMap::new();
    for o in &outputs {
        by_shared_build
            .entry(o.derivation)
            .or_default()
            .push(o.hash.as_ref().is_some_and(|h| hits.contains_key(h)) || cached.contains(&o.id));
    }

    let served: Vec<DerivationId> = touched
        .iter()
        .copied()
        .filter(|d| {
            by_shared_build
                .get(d)
                .is_some_and(|outs| !outs.is_empty() && outs.iter().all(|served| *served))
        })
        .collect();
    let newly_cache_available = flip_cache_available(db, &served).await?;

    let seeded =
        gradient_db::graph::runtime_can_start::seed_runtime_deps(txn, &[], &touched).await?;
    if !seeded.complete.is_empty() {
        let lock = gradient_db::graph::can_start::lock_shared_builds(txn, &seeded.complete).await?;
        let changes = gradient_db::graph::can_start::became_fetchable(&lock).await?;
        gradient_db::status::emit_transition_effects(ctx, &changes).await?;
    }

    let mut roots = touched;
    roots.extend_from_slice(&newly_cache_available);
    roots.sort_unstable();
    roots.dedup();

    let mut changes = Vec::new();
    let mut gained_need = Vec::new();
    for chunk in roots.chunks(gradient_db::IN_CHUNK_SIZE) {
        let settled = gradient_db::graph::can_start::update_and_settle_need(db, chunk)
            .await
            .context("settle what an upstream hit needs built")?;
        changes.extend(settled.changes);
        gained_need.extend_from_slice(&settled.moved.gained);
    }
    changes
        .extend(gradient_db::graph::can_start::settle_created(db, &newly_cache_available).await?);
    gradient_db::status::emit_transition_effects(ctx, &changes).await?;

    Ok(gained_need)
}

pub(crate) async fn mark_probed(
    ctx: &DbContext,
    shared_builds: &[DerivationId],
) -> Result<Vec<DerivationId>> {
    if shared_builds.is_empty() {
        return Ok(Vec::new());
    }

    let db = &ctx.worker_db;
    let mut answered: Vec<DerivationId> = Vec::new();
    for chunk in shared_builds.chunks(gradient_db::IN_CHUNK_SIZE) {
        let ids: Vec<uuid::Uuid> = chunk.iter().map(|d| d.into_inner()).collect();
        let rows = db
            .query_all_raw(MARK_PROBED.bind([ids.into()]))
            .await
            .context("record the shared builds the upstream probe answered for")?;
        answered.extend(
            rows.iter()
                .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
                .map(DerivationId::new),
        );
    }

    let mut changes = Vec::new();
    let mut gained_need = Vec::new();
    for chunk in answered.chunks(gradient_db::IN_CHUNK_SIZE) {
        let settled = gradient_db::graph::can_start::update_and_settle_need(db, chunk)
            .await
            .context("settle what an answered shared build needs built")?;
        changes.extend(settled.changes);
        gained_need.extend_from_slice(&settled.moved.gained);
        changes.extend(
            gradient_db::graph::can_start::promote(db, chunk)
                .await
                .context("queue the shared builds the answer made promotable")?,
        );
    }
    gradient_db::status::emit_transition_effects(ctx, &changes).await?;

    Ok(gained_need)
}

async fn persist_narinfo(
    db: &WorkerDb,
    rows: &[MDerivationOutput],
    hits: &HashMap<String, UpstreamHit>,
) -> Result<()> {
    let mut learned: Vec<(DerivationId, Vec<String>)> = Vec::new();
    for o in rows {
        let Some(hit) = o.hash.as_ref().and_then(|h| hits.get(h)) else {
            continue;
        };

        if let Some(references) = hit.references.as_deref() {
            let tokens: Vec<String> = references.split_whitespace().map(str::to_owned).collect();
            if !tokens.is_empty() {
                learned.push((o.derivation, tokens));
            }
        }

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
            error!(hash = ?o.hash, error = %e, "failed to persist upstream availability");
        }
    }

    for (derivation, tokens) in &learned {
        let producers =
            gradient_db::graph::runtime_dependencies::producers_of_tokens(db, tokens).await?;
        gradient_db::graph::runtime_dependencies::insert_runtime_dependencies(
            db,
            *derivation,
            &producers,
        )
        .await?;
    }

    Ok(())
}

#[tracing::instrument(level = "debug", skip_all)]
pub(crate) async fn after_commit(
    ctx: &DbContext,
    writer: &ractor::ActorRef<crate::writer::GraphMsg>,
    batch: &RecordBatch,
    report: &RecordReport,
) {
    if report.skipped {
        return;
    }

    if let Some(task_id) = batch.task {
        if let Err(e) = gradient_db::status::announce_entry_point_statuses(
            ctx,
            report.evaluation,
            &report.entry_points,
        )
        .await
        {
            error!(error = %e, evaluation = %report.evaluation, "failed to report the entry points");
        }
        if let Ok(Some(task)) = ETask::find_by_id(task_id).one(&ctx.worker_db).await {
            let gc_ctx = ctx.detached();
            let keep = task.keep_evaluations as usize;
            let writer = writer.clone();
            ctx.shutdown.spawn(async move {
                if let Err(e) = crate::gc::gc_task_evaluations(&gc_ctx, writer, task_id, keep).await
                {
                    error!(error = %e, %task_id, "GC: per-task evaluation GC failed");
                }
            });
        }
    }

    ctx.memory.probe_requests.send(report.to_probe.clone());

    ctx.events.publish(evaluation::Progress {
        evaluation_id: report.evaluation,
        task: batch.task,
    });
}

pub(crate) async fn fail_evaluation(ctx: &DbContext, evaluation: EvaluationId, message: &str) {
    if let Ok(Some(eval)) = EEvaluation::find_by_id(evaluation)
        .one(&ctx.worker_db)
        .await
        && let Err(e) = update_evaluation_status_with_error(
            ctx,
            eval,
            EvaluationStatus::Failed,
            &EvaluationFailure::GraphInsert(message),
        )
        .await
    {
        error!(error = %e, %evaluation, "failed to fail the evaluation");
    }
}

fn drv_hash_name(drv_path: &str) -> Option<(String, String)> {
    let sp = StorePath::parse_stored(drv_path).ok()?;
    let name = sp.name().strip_suffix(".drv")?;
    Some((sp.hash().to_owned(), name.to_owned()))
}

fn output_hash_name(path: &str) -> Option<(String, String)> {
    let sp = StorePath::parse_stored(path).ok()?;
    Some((sp.hash().to_owned(), sp.name().to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_held_evaluation_takes_batches_after_its_stream_ended() {
        assert!(accepts_batches(
            EvaluationStatus::EvaluatingDerivation,
            false
        ));
        assert!(accepts_batches(EvaluationStatus::Building, true));
        assert!(accepts_batches(EvaluationStatus::Waiting, true));
        assert!(!accepts_batches(EvaluationStatus::Building, false));
        assert!(!accepts_batches(EvaluationStatus::Aborted, true));
    }
}
