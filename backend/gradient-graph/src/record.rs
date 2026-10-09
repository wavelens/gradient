/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::events::evaluation;
use std::collections::{HashMap, HashSet};

use anyhow::{Context, Result, anyhow};
use gradient_db::{
    DbContext, WorkerDb,
    status::{record_evaluation_message, update_evaluation_status_with_error},
};
use gradient_entity::StorePath;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::evaluation_message::MessageLevel;
use gradient_types::*;
use gradient_wire::types::DiscoveredDerivation;
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
UPDATE derivation_build SET probed = true, updated_at = (now() AT TIME ZONE 'UTC')
WHERE derivation = ANY($1::uuid[]) AND NOT probed
RETURNING derivation
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
            input_counts.push(d.dependencies.iter().collect::<HashSet<_>>().len() as i32);
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
                let (hash, package) = output_hash_name(&output.path).unwrap_or_else(|| {
                    (
                        gradient_entity::derivation_output::UNKNOWN_OUTPUT_HASH.to_owned(),
                        output.name.clone(),
                    )
                });
                outputs.push(
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
                return Err(anyhow::Error::new(e).context("failed to insert derivation outputs"));
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
    async fn resolve_shared_builds(
        &self,
        ids: &HashMap<String, DerivationId>,
        derivations: &[DiscoveredDerivation],
        batch: &RecordBatch,
    ) -> Result<()> {
        let now = gradient_types::now();
        let all_ids: Vec<DerivationId> = ids
            .values()
            .copied()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let truly: HashSet<DerivationId> = batch
            .truly_substituted
            .iter()
            .filter_map(|p| ids.get(p).copied())
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
                let status = if truly.contains(&drv_id) {
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
                return Err(anyhow::Error::new(e).context("failed to upsert shared builds"));
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
                return Err(anyhow::Error::new(e).context("failed to upsert build_job rows"));
            }
        }

        if !truly.is_empty() {
            let truly_ids: Vec<DerivationId> = truly.iter().copied().collect();
            let changes =
                gradient_db::graph::promotion::substitute_created_shared_builds(db, &truly_ids)
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
        batch: &RecordBatch,
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
        locked.extend(
            batch
                .truly_substituted
                .iter()
                .filter_map(|p| resolved.by_path.get(p).copied()),
        );
        locked.sort_unstable();
        locked.dedup();
        if locked.is_empty() {
            return Ok(Vec::new());
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
                let Ok(store_path) = StorePath::parse(src) else {
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

    #[tracing::instrument(level = "debug", skip_all)]
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
            if let Err(e) = insert_entry_points(chunk.to_vec())
                .exec(&self.ctx.worker_db)
                .await
                && !matches!(e, sea_orm::DbErr::RecordNotInserted)
            {
                error!(error = %e, "failed to insert entry points");
            }
        }

        entry_point_drvs
    }
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
    match EEvaluation::find_by_id(evaluation_id)
        .one(&ctx.worker_db)
        .await
        .context("fetch evaluation")?
    {
        Some(e) if !accepts_batches(e.status, ctx.held_evaluations.holds(evaluation_id)) => {
            warn!(%evaluation_id, status = ?e.status, "batch for an evaluation that is not streaming; dropped as stale");
            return Ok(RecordReport {
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
        writer
            .resolve_shared_builds(ids, &batch.derivations, batch)
            .await?;
        writer.add_system_features(&batch.derivations, ids).await;
        let adopted = writer.adopt_references(&resolved, &newly_walked).await?;
        report.entry_points = match batch.task {
            Some(task) => {
                writer
                    .process_entry_points(task, &batch.derivations, ids)
                    .await
            }
            None => Vec::new(),
        };
        report.to_probe = writer
            .advance_can_start(
                batch,
                &resolved,
                &newly_walked,
                &grew,
                &adopted,
                &report.entry_points,
            )
            .await?;
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

    let hashes: Vec<String> = hits.keys().cloned().collect();
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
    let mut by_shared_build: HashMap<DerivationId, Vec<bool>> = HashMap::new();
    for o in &outputs {
        by_shared_build
            .entry(o.derivation)
            .or_default()
            .push(hits.contains_key(&o.hash) || o.is_cached_anywhere());
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
    for o in rows.iter().filter(|o| !o.is_cached_anywhere()) {
        let Some(hit) = hits.get(&o.hash) else {
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
            error!(hash = %o.hash, error = %e, "failed to persist upstream availability");
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

    ctx.probe_requests.send(report.to_probe.clone());

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
            message.to_owned(),
            Some("db-insert".to_string()),
        )
        .await
    {
        error!(error = %e, %evaluation, "failed to fail the evaluation");
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use gradient_entity::evaluation::EvaluationStatus;
    use sea_orm::{
        DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Statement,
        TransactionTrait, Value,
    };
    use std::collections::BTreeMap;

    async fn apply(ctx: &DbContext, batch: &RecordBatch) -> Result<RecordReport> {
        let tx = std::sync::Arc::new(ctx.worker_db.begin().await.expect("begin"));
        let scoped = ctx.in_transaction(std::sync::Arc::clone(&tx));
        let report = apply_batch(&scoped, batch).await;
        drop(scoped);
        std::sync::Arc::try_unwrap(tx)
            .expect("no handle outlives the call")
            .commit()
            .await
            .expect("commit");

        report
    }

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

    #[test]
    fn a_build_edge_landing_on_a_runtime_edge_becomes_both_and_reseeds_its_parent() {
        let sql = EDGE_INSERT.text();
        assert!(
            sql.contains(
                "ON CONFLICT (derivation, dependency) DO UPDATE SET kind = 2 WHERE derivation_dependency.kind = 1\nRETURNING derivation"
            ),
            "a runtime edge recorded before its parent was walked again must start counting as a build input, \
             and the parent must come back in the grown set so both counters are recounted: {sql}"
        );
    }

    #[test]
    fn a_rewalked_entry_point_is_kept_not_duplicated() {
        let row = MEntryPoint {
            id: EntryPointId::now_v7(),
            eval: "packages.x86_64-linux.hello".to_string(),
            ..Default::default()
        }
        .into_active_model();

        let sql =
            sea_orm::QueryTrait::build(&insert_entry_points(vec![row]), DatabaseBackend::Postgres)
                .to_string();

        assert!(
            sql.contains(r#"ON CONFLICT ("evaluation", "eval") DO NOTHING"#),
            "{sql}"
        );
    }

    #[tokio::test]
    async fn what_the_batch_needs_reaches_the_upstream_probe() {
        let gained = DerivationId::now_v7();
        let (ctx, _pool, mut probes) = crate::test_ctx::ctx_with_probes(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        )
        .await;
        let writer = crate::Graph::new()
            .spawn(ctx.clone(), None, None)
            .await
            .expect("the graph writer starts");

        after_commit(
            &ctx,
            &writer,
            &RecordBatch {
                evaluation: EvaluationId::now_v7(),
                ..Default::default()
            },
            &RecordReport {
                to_probe: vec![gained],
                ..Default::default()
            },
        )
        .await;

        assert_eq!(
            probes
                .try_recv()
                .expect("the batch's newly wanted builds reach the probe"),
            vec![gained],
        );
        writer.stop_and_wait(None, None).await.unwrap();
    }

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b.drv";

    fn drv(path: &str, deps: &[&str]) -> DiscoveredDerivation {
        DiscoveredDerivation {
            drv_path: path.to_owned(),
            dependencies: deps.iter().map(|d| (*d).to_owned()).collect(),
            architecture: "x86_64-linux".to_owned(),
            allow_substitutes: true,
            ..Default::default()
        }
    }

    fn derivation_row(path: &str, walked: bool) -> MDerivation {
        let (hash, name) = drv_hash_name(path).unwrap();
        MDerivation {
            id: DerivationId::now_v7(),
            hash,
            name,
            walked,
            ..Default::default()
        }
    }

    fn shared_build_row(derivation: DerivationId) -> MDerivationBuild {
        MDerivationBuild {
            id: DerivationBuildId::now_v7(),
            derivation,
            ..Default::default()
        }
    }

    fn hash_row(hash: &str) -> BTreeMap<String, Value> {
        BTreeMap::from([("hash".to_owned(), Value::from(hash.to_owned()))])
    }

    fn need_row(derivation: DerivationId, wanted: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "derivation".to_owned(),
                Value::from(derivation.into_inner()),
            ),
            ("wanted".to_owned(), Value::from(wanted)),
        ])
    }

    fn drv_row(derivation: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([(
            "derivation".to_owned(),
            Value::from(derivation.into_inner()),
        )])
    }

    fn ripple_row(derivation: DerivationId, startable: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "derivation".to_owned(),
                Value::from(derivation.into_inner()),
            ),
            ("startable".to_owned(), Value::from(startable)),
        ])
    }

    fn transition_row(derivation: DerivationId, from: i32, to: i32) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "derivation".to_owned(),
                Value::from(derivation.into_inner()),
            ),
            ("from_status".to_owned(), Value::from(from)),
            ("to_status".to_owned(), Value::from(to)),
        ])
    }

    fn completeness_row(
        derivation: DerivationId,
        was_complete: bool,
        complete: bool,
    ) -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("id".to_owned(), Value::from(derivation.into_inner())),
            ("was_complete".to_owned(), Value::from(was_complete)),
            ("complete".to_owned(), Value::from(complete)),
        ])
    }

    fn ok(n: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: n,
        }
    }

    fn scripted(evaluation: EvaluationId) -> (MEvaluation, MDerivation, MDerivation) {
        let eval = MEvaluation {
            id: evaluation,
            status: EvaluationStatus::EvaluatingDerivation,
            ..Default::default()
        };
        (eval, derivation_row(A, true), derivation_row(B, false))
    }

    fn walk_of_a(eval: MEvaluation, a: &MDerivation, b: &MDerivation) -> DatabaseConnection {
        scripted_walk_of_a(eval, a, b)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection()
    }

    fn scripted_walk_of_a(eval: MEvaluation, a: &MDerivation, b: &MDerivation) -> MockDatabase {
        MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id), shared_build_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
    }

    #[tokio::test]
    async fn a_batch_claims_the_cached_outputs_of_its_jobs_for_its_projects_caches() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let (cached_path, cache) = (
            gradient_types::ids::CachedPathId::now_v7(),
            gradient_types::ids::CacheId::now_v7(),
        );
        let claim = BTreeMap::from([
            (
                "cached_path".to_owned(),
                Value::from(cached_path.into_inner()),
            ),
            (
                "hash".to_owned(),
                Value::from("cccccccccccccccccccccccccccccccc"),
            ),
            ("package".to_owned(), Value::from("b-1.0")),
            (
                "nar_hash".to_owned(),
                Value::from(Some("sha256:def".to_owned())),
            ),
            ("nar_size".to_owned(), Value::from(Some(5_i64))),
            ("references".to_owned(), Value::from(None::<String>)),
            ("cache".to_owned(), Value::from(cache.into_inner())),
        ]);
        let db = scripted_walk_of_a(eval, &a, &b)
            .append_query_results([vec![claim]])
            .append_query_results([vec![MCache {
                id: cache,
                ..Default::default()
            }]])
            .append_exec_results([ok(1)])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        drop(ctx);

        let log = gradient_db::pool::raw_statements(pool.into_transaction_log());
        let at = |needle: &str| {
            log.iter()
                .position(|s| s.sql.contains(needle))
                .unwrap_or_else(|| panic!("{needle} must run: {log:?}"))
        };
        let jobs = at("INSERT INTO \"build_job\"");
        let claims = at("WITH claim AS");
        let signatures = at("INSERT INTO \"cached_path_signature\"");
        assert!(
            jobs < claims && claims < signatures,
            "the claim reads the jobs the batch inserted: {log:?}"
        );
        let bound = format!("{:?}", log[claims].values);
        for id in [evaluation.to_string(), a.id.to_string(), b.id.to_string()] {
            assert!(bound.contains(&id), "{id} is not bound: {bound}");
        }
        assert!(
            format!("{:?}", log[signatures].values).contains(&cached_path.to_string()),
            "{:?}",
            log[signatures]
        );
    }

    #[tokio::test]
    async fn a_walked_shared_build_reaches_the_upstream_probe() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let (ctx, _pool) = ctx(walk_of_a(eval, &a, &b)).await;

        let report = apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert!(report.to_probe.contains(&a.id), "{report:?}");
        assert!(!report.to_probe.contains(&b.id), "{report:?}");
    }

    #[tokio::test]
    async fn a_named_dependency_gets_a_stub_before_its_edge() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = walk_of_a(eval, &a, &b);
        let (ctx, pool) = ctx(db).await;

        let report = apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(report.walked, 1);
        drop(ctx);
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        assert_eq!(
            log.len(),
            21,
            "evaluation, walked, stubs, resolve, edges, walk lock, walk seed, shared build insert, shared build select, jobs, reference adoption, lock, mark, seed, promote, unpromote, version, the raised, locked need update, and the claim of the cached outputs: {log:?}"
        );
        let walked = log
            .iter()
            .position(|s| s.contains("walked = true") && s.contains("WHERE NOT derivation.walked"))
            .expect("the walked upsert runs");
        let stub = log
            .iter()
            .position(|s| s.contains("ON CONFLICT (hash, name) DO NOTHING"))
            .expect("the stub insert runs");
        let edge = log
            .iter()
            .position(|s| s.contains("INSERT INTO derivation_dependency"))
            .expect("the edge insert runs");
        let lock = log
            .iter()
            .position(|s| s.contains("ORDER BY derivation FOR NO KEY UPDATE"))
            .expect("the can-start pass locks its shared builds");
        let mark = log
            .iter()
            .position(|s| s.contains("SET fetchable = true"))
            .expect("the mark runs");
        let seed = log
            .iter()
            .position(|s| s.contains("SET blocking_deps = (SELECT count(*)"))
            .expect("the seed runs");
        assert!(
            walked < stub && stub < edge && edge < lock,
            "walked, then stubs, then edges, and only then the counters: {log:?}"
        );
        assert!(
            lock < mark && mark < seed,
            "the lock precedes the flip, and the absolute seed is the last word on the batch's own rows: {log:?}"
        );
        let shared_builds = log
            .iter()
            .position(|s| s.contains(r#"INSERT INTO \"derivation_build\""#))
            .expect("the shared build insert runs");
        let adopt = log
            .iter()
            .position(|s| s.contains("&& t.tokens"))
            .expect("the reference adoption runs");
        assert!(
            shared_builds < adopt && adopt < lock,
            "a parent's recount reads its new dependency's shared build, so adoption follows the shared build insert: {log:?}"
        );
        assert!(
            log[stub].contains(&b.hash) && !log[walked].contains(&b.hash),
            "the dependency is a stub, not a walked row: {log:?}"
        );
        assert!(
            log[edge].contains("RETURNING derivation"),
            "the seed set is the edges that actually landed: {log:?}"
        );
    }

    #[tokio::test]
    async fn an_upstream_hit_writes_the_runtime_dependencies_its_narinfo_names() {
        let derivation = DerivationId::now_v7();
        let out = MDerivationOutput {
            id: gradient_types::ids::DerivationOutputId::now_v7(),
            derivation,
            hash: "cccccccccccccccccccccccccccccccc".to_owned(),
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![out.clone()]])
            .append_query_results([vec![out.clone()]])
            .append_query_results([vec![drv_row(DerivationId::now_v7())]])
            .append_query_results([vec![out]])
            .append_query_results([vec![drv_row(derivation)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 4])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let tx = std::sync::Arc::new(ctx.worker_db.begin().await.expect("begin"));
        let scoped = ctx.in_transaction(std::sync::Arc::clone(&tx));
        apply_upstream_hits(
            &scoped,
            &HashMap::from([(
                "cccccccccccccccccccccccccccccccc".to_owned(),
                UpstreamHit {
                    references: Some("dddddddddddddddddddddddddddddddd-dep".to_owned()),
                    ..Default::default()
                },
            )]),
        )
        .await
        .expect("the hit applies");
        drop(scoped);
        std::sync::Arc::try_unwrap(tx)
            .expect("no handle outlives the call")
            .commit()
            .await
            .expect("commit");
        drop(ctx);

        let log = gradient_db::pool::statements(pool.into_transaction_log());
        for fragment in [
            "INSERT INTO derivation_dependency (derivation, dependency, kind)",
            "SET cache_available = true",
            "SET missing_runtime_deps = x.n",
            "region(evaluation, derivation, builder) AS",
        ] {
            assert!(
                log.iter().any(|s| s.contains(fragment)),
                "{fragment} never ran: {log:?}"
            );
        }
    }

    #[tokio::test]
    async fn answering_a_shared_build_needs_what_it_will_be_built_from() {
        let shared_build = DerivationId::now_v7();
        let input = DerivationId::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![drv_row(shared_build)]])
            .append_query_results([vec![need_row(input, true)]])
            .append_query_results([vec![need_row(input, true)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 4])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let gained = mark_probed(&ctx, &[shared_build])
            .await
            .expect("the round lands");
        assert_eq!(gained, vec![input], "the input the miss needs built");

        drop(ctx);
        let raw = gradient_db::pool::raw_statements(pool.into_transaction_log());
        let last = raw.last().expect("statements ran");
        assert!(
            last.sql.contains("SET status =")
                && format!("{:?}", last.values).contains(&shared_build.to_string()),
            "the answered shared build itself is offered to promotion last: {last:?}"
        );
        let log: Vec<String> = raw.iter().map(|s| s.sql.clone()).collect();
        for fragment in [
            "SET probed = true",
            "region(evaluation, derivation, builder) AS",
        ] {
            assert!(
                log.iter().any(|s| s.contains(fragment)),
                "{fragment} never ran: {log:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_shared_build_already_answered_costs_one_statement() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        assert!(
            mark_probed(&ctx, &[DerivationId::now_v7()])
                .await
                .expect("the round lands")
                .is_empty()
        );

        drop(ctx);
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        assert_eq!(log.len(), 1, "one write and nothing else: {log:?}");
    }

    #[tokio::test]
    async fn a_batch_marks_nothing_cache_available_on_its_own() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id), shared_build_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        drop(ctx);
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        assert!(
            !log.iter().any(|s| s.contains("SET cache_available = true")),
            "{log:?}"
        );
    }

    #[tokio::test]
    async fn a_walk_settles_the_subtree_bit_before_it_touches_a_shared_build() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id), shared_build_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                ..Default::default()
            },
        )
        .await
        .unwrap();
        drop(ctx);

        let log = gradient_db::pool::raw_statements(pool.into_transaction_log());
        let upsert = log
            .iter()
            .position(|s| s.sql == WALKED_UPSERT.text())
            .expect("the walked upsert runs");
        assert!(
            log[upsert]
                .sql
                .contains("unwalked_inputs = EXCLUDED.unwalked_inputs"),
            "the record lands with its input count: {}",
            log[upsert].sql
        );
        assert!(
            format!("{:?}", log[upsert].values).contains("Int(Some(1))"),
            "A names one input: {:?}",
            log[upsert].values
        );
        let seed = log
            .iter()
            .position(|s| {
                s.sql
                    .contains("UPDATE derivation d SET unwalked_inputs = x.n")
            })
            .expect("the subtree seed runs");
        let edges = log
            .iter()
            .position(|s| s.sql.contains("INSERT INTO derivation_dependency"))
            .expect("the edge insert runs");
        let shared_build_lock = log
            .iter()
            .position(|s| {
                s.sql.contains("FROM derivation_build") && s.sql.contains("FOR NO KEY UPDATE")
            })
            .expect("the can-start pass locks its shared builds");
        assert!(
            edges < seed && seed < shared_build_lock,
            "edges, then the seed, then shared builds: {log:?}"
        );
    }

    #[tokio::test]
    async fn the_cross_evaluation_bump_names_each_derivation_once() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([vec![drv_row(a.id), drv_row(a.id)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id), shared_build_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 7])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        drop(ctx);
        let log: Vec<Statement> = pool
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .collect();
        let bump = log
            .iter()
            .find(|s| {
                s.sql
                    .contains("SELECT DISTINCT evaluation FROM build_job WHERE derivation = ANY")
            })
            .expect("a batch that grew an edge bumps the evaluations sharing it");
        let Some(Value::Array(_, Some(bound))) =
            bump.values.as_ref().and_then(|v| v.0.first()).cloned()
        else {
            panic!("the bump binds one uuid array: {:?}", bump.values);
        };

        assert_eq!(
            bound.len(),
            1,
            "two edges on one derivation bind it once: {bound:?}"
        );
        let Some(Value::Uuid(Some(only))) = bound.first().cloned() else {
            panic!("the bump binds uuids: {bound:?}");
        };

        assert_eq!(
            only,
            a.id.into_inner(),
            "and the one it keeps is the derivation the edges named"
        );
    }

    #[tokio::test]
    async fn the_start_pass_marks_ripples_promotes_seeds_then_settles_the_queue() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id), shared_build_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![drv_row(b.id)]])
            .append_query_results([vec![ripple_row(a.id, true)]])
            .append_query_results([vec![drv_row(a.id)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![transition_row(a.id, 1, 0)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                truly_substituted: HashSet::from([B.to_owned()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        drop(ctx);
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        let at = |needle: &str| {
            log.iter()
                .position(|s| s.contains(needle))
                .unwrap_or_else(|| panic!("{needle} must run: {log:?}"))
        };
        let mark = at("SET fetchable = true");
        let ripple = at("blocking_deps - c.n");
        let seed = at("SET blocking_deps = (SELECT count(*)");
        let unpromote = at("ANY($1::uuid[]) AND NOT (");
        let promotes: Vec<usize> = log
            .iter()
            .enumerate()
            .filter(|(_, s)| s.contains("queued_at = coalesce(db.queued_at"))
            .map(|(i, _)| i)
            .collect();

        assert_eq!(
            promotes.len(),
            2,
            "the ripple promotes, then the seed does: {log:?}"
        );
        assert!(
            mark < ripple && ripple < promotes[0] && promotes[0] < seed,
            "the flip and its ripple settle before the absolute seed: {log:?}"
        );
        assert!(
            seed < promotes[1] && promotes[1] < unpromote,
            "the seed's own promote and the un-promote that undoes a stale-low queueing come last: {log:?}"
        );
    }

    #[tokio::test]
    async fn an_adopting_parent_is_seeded_once_after_the_mark() {
        let evaluation = EvaluationId::now_v7();
        let parent = DerivationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id), shared_build_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![drv_row(parent)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![drv_row(b.id)]])
            .append_query_results([vec![ripple_row(a.id, true)]])
            .append_query_results([vec![drv_row(a.id)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![transition_row(a.id, 1, 0)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 10])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                truly_substituted: HashSet::from([B.to_owned()]),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        drop(ctx);
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        let mark = log
            .iter()
            .position(|s| s.contains("SET fetchable = true"))
            .unwrap_or_else(|| panic!("the mark runs: {log:?}"));
        let seeds: Vec<usize> = log
            .iter()
            .enumerate()
            .filter(|(_, s)| s.contains("SET blocking_deps = (SELECT count(*)"))
            .map(|(i, _)| i)
            .collect();

        assert_eq!(seeds.len(), 1, "one absolute seed per batch: {log:?}");
        assert!(mark < seeds[0], "the seed follows the mark: {log:?}");
        assert!(
            log[seeds[0]].contains(&parent.into_inner().to_string()),
            "the adopting parent is seeded with the batch: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_batch_promotes_what_its_new_builders_need() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id), shared_build_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![need_row(b.id, true)], vec![need_row(b.id, true)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![drv_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<MEntryPoint>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        drop(ctx);
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        let walk = log
            .iter()
            .position(|s| s.contains("ON d.derivation = r.derivation ORDER BY r.derivation"))
            .expect("the batch updates what it needs built");
        let need = log
            .iter()
            .position(|s| s.contains("SET wanted ="))
            .expect("the batch writes what the update answered");
        let seed = log
            .iter()
            .position(|s| s.contains("SET blocking_deps = (SELECT count(*)"))
            .expect("the seed runs");
        let promote = log
            .iter()
            .enumerate()
            .filter(|(_, s)| s.contains("queued_at = coalesce(db.queued_at"))
            .map(|(i, _)| i)
            .next_back()
            .expect("the wanted passthrough is promoted");

        assert!(
            seed < walk && walk < need && need < promote,
            "need settles after the counters, and its promote reads the settled gate: {log:?}"
        );
        assert!(
            log[walk].contains("wanted(evaluation, derivation, builder) AS"),
            "the update is the closure walk, not one hop: {log:?}"
        );
        assert!(
            log[promote].contains("AND db.wanted"),
            "the promote reads the column the update just wrote: {log:?}"
        );
    }

    #[tokio::test]
    async fn a_redelivered_batch_flips_nothing_and_only_repeats_guarded_writes() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([Vec::<MDerivation>::new()])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id), shared_build_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(0); 2])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let report = apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &[B])],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(
            report.walked, 0,
            "an already walked derivation is not a flip"
        );
        drop(ctx);
        let log = gradient_db::pool::statements(pool.into_transaction_log());
        let writes: Vec<&String> = log
            .iter()
            .filter(|s| s.contains("INSERT INTO derivation"))
            .collect();
        assert!(
            writes
                .iter()
                .any(|s| s.contains("INSERT INTO derivation_dependency")),
            "the batch re-asserts its edges even though it flipped nothing: {log:?}"
        );
        assert!(
            writes.iter().all(|s| s.contains("ON CONFLICT")),
            "every graph write a re-delivered batch repeats lands on no row: {writes:?}"
        );
        assert!(
            !log.iter().any(|s| s.contains("SET blocking_deps = ")),
            "a batch that flipped nothing walked and grew no edge has no counter to move: {log:?}"
        );
    }

    #[tokio::test]
    async fn an_unparseable_dependency_fails_the_batch() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, _) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .into_connection();
        let (ctx, _pool) = ctx(db).await;

        let err = apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![drv(A, &["not-a-store-path"])],
                ..Default::default()
            },
        )
        .await
        .expect_err("an unparseable dependency fails the batch");

        let msg = err.to_string();
        assert!(
            msg.contains(A) && msg.contains("not-a-store-path"),
            "the failure names the derivation and the offending path: {msg}"
        );
    }

    #[tokio::test]
    async fn an_empty_batch_touches_only_the_evaluation() {
        let evaluation = EvaluationId::now_v7();
        let (eval, _, _) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let report = apply(
            &ctx,
            &RecordBatch {
                evaluation,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(report.walked, 0);
        drop(ctx);
        assert_eq!(pool.into_transaction_log().len(), 1);
    }

    #[tokio::test]
    async fn a_walked_record_writes_its_limits_onto_an_existing_shared_build() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, _) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let mut record = drv(A, &[]);
        record.timeout_secs = Some(3600);
        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![record],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        drop(ctx);
        let log: Vec<Statement> = pool
            .into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .collect();
        let shared_builds = log
            .iter()
            .position(|s| s.sql.contains("INSERT INTO \"derivation_build\""))
            .expect("the shared build insert runs");
        let limits = log
            .iter()
            .position(|s| s.sql.contains("UPDATE derivation_build AS db"))
            .expect("the limits update runs");
        let seed = log
            .iter()
            .position(|s| s.sql.contains("SET blocking_deps = (SELECT count(*)"))
            .expect("the can-start seed runs");
        assert!(
            shared_builds < limits && limits < seed,
            "limits are written once the shared build exists, and the counters after both: {log:?}"
        );
        assert!(
            format!("{:?}", log[limits].values).contains("BigInt(Some(3600))"),
            "the update carries the record's limits: {:?}",
            log[limits]
        );
    }

    async fn statements_of_a_walk_of(record: DiscoveredDerivation) -> Vec<Statement> {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, _) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![shared_build_row(a.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results(vec![Vec::<BTreeMap<String, Value>>::new(); 6])
            .append_exec_results(vec![ok(1); 7])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply(
            &ctx,
            &RecordBatch {
                evaluation,
                derivations: vec![record],
                ..Default::default()
            },
        )
        .await
        .unwrap();

        drop(ctx);
        pool.into_transaction_log()
            .iter()
            .flat_map(|t| t.statements().to_vec())
            .collect()
    }

    #[tokio::test]
    async fn an_imported_record_marks_its_derivation_and_a_plain_record_does_not() {
        let mark = MARK_IFD.text();
        let mut imported = drv(A, &[]);
        imported.ifd = true;

        let log = statements_of_a_walk_of(imported).await;
        let upsert = log
            .iter()
            .position(|s| s.sql.contains("INSERT INTO derivation\n"))
            .expect("the walked upsert runs");
        let marked = log
            .iter()
            .position(|s| s.sql == mark)
            .unwrap_or_else(|| panic!("an imported record marks its derivation: {log:?}"));
        let (hash, _) = drv_hash_name(A).unwrap();
        assert!(
            upsert < marked,
            "the mark lands on the upserted row: {log:?}"
        );
        assert!(
            format!("{:?}", log[marked].values).contains(&hash),
            "{:?}",
            log[marked]
        );

        let plain = statements_of_a_walk_of(drv(A, &[])).await;
        assert!(
            !plain.iter().any(|s| s.sql == mark),
            "a plain record leaves the flag alone: {plain:?}"
        );
    }
}
