/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Writing one worker batch of discovered derivations into the graph: a stub
//! row for every dependency it names, the walked records, the outputs and edges
//! of every derivation it reports, the anchors and this evaluation's jobs, all
//! inside the actor's transaction.

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
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait,
    IntoActiveModel, QueryFilter, Statement, TransactionTrait, Value,
};
use tracing::{debug, error, warn};

use crate::messages::{IngestBatch, IngestReport, UpstreamHit};

const BATCH_SIZE: usize = 1000;

/// Insert or complete the record of every derivation the worker walked. The
/// conflict update runs only for a row that is not yet walked, so RETURNING
/// yields exactly the derivations this batch flipped.
const WALKED_UPSERT: &str = r#"
INSERT INTO derivation
    (id, hash, name, architecture, pname, prefer_local_build, is_fixed_output, allow_substitutes, walked, created_at)
SELECT d.id, d.hash, d.name, d.architecture, NULLIF(d.pname, ''), d.prefer_local_build,
       d.is_fixed_output, d.allow_substitutes, true, $9
FROM unnest($1::uuid[], $2::text[], $3::text[], $4::text[], $5::text[], $6::bool[], $7::bool[], $8::bool[])
     AS d(id, hash, name, architecture, pname, prefer_local_build, is_fixed_output, allow_substitutes)
ON CONFLICT (hash, name) DO UPDATE SET
    architecture = EXCLUDED.architecture,
    pname = EXCLUDED.pname,
    prefer_local_build = EXCLUDED.prefer_local_build,
    is_fixed_output = EXCLUDED.is_fixed_output,
    allow_substitutes = EXCLUDED.allow_substitutes,
    walked = true
WHERE NOT derivation.walked
RETURNING hash
"#;

/// A row for every dependency the batch names, so its edge can land now. A
/// stub carries only what the path itself says; the walk fills the rest.
const STUB_INSERT: &str = r#"
INSERT INTO derivation (id, hash, name, architecture, walked, created_at)
SELECT d.id, d.hash, d.name, '', false, $4
FROM unnest($1::uuid[], $2::text[], $3::text[]) AS d(id, hash, name)
ON CONFLICT (hash, name) DO NOTHING
"#;

const RESOLVE_IDS: &str = "SELECT id, hash FROM derivation WHERE hash = ANY($1::text[])";

/// `RETURNING` names exactly the dependents whose edge set GREW, so the readiness
/// seed runs for those and not for every derivation the batch mentions.
const EDGE_INSERT: &str = r#"
INSERT INTO derivation_dependency (derivation, dependency)
SELECT e.derivation, e.dependency FROM unnest($1::uuid[], $2::uuid[]) AS e(derivation, dependency)
ON CONFLICT DO NOTHING
RETURNING derivation
"#;

/// A stub's anchor exists before the record that carries the derivation's
/// limits, so they land here; `0` stands in for an unset limit in the arrays.
const ANCHOR_LIMITS_UPDATE: &str = r#"
UPDATE derivation_build AS db
SET timeout_secs = NULLIF(l.timeout_secs, 0), max_silent_secs = NULLIF(l.max_silent_secs, 0)
FROM unnest($1::uuid[], $2::bigint[], $3::bigint[]) AS l(derivation, timeout_secs, max_silent_secs)
WHERE db.derivation = l.derivation
  AND (db.timeout_secs, db.max_silent_secs)
      IS DISTINCT FROM (NULLIF(l.timeout_secs, 0), NULLIF(l.max_silent_secs, 0))
"#;

/// Every drv path the batch names, resolved twice: by path for the writes that
/// key on what the worker reported, by hash for the readiness seed, whose input
/// is the walked-upsert's `RETURNING hash`.
struct Resolved {
    by_path: HashMap<String, DerivationId>,
    by_hash: HashMap<String, DerivationId>,
}

/// Writes a single batch of discovered derivations, inside the actor's
/// transaction. Holds what every step shares: the scoped context and the
/// evaluation the batch belongs to.
struct BatchWriter<'a> {
    ctx: &'a DbContext,
    evaluation_id: EvaluationId,
}

impl BatchWriter<'_> {
    fn db(&self) -> &WorkerDb {
        &self.ctx.worker_db
    }

    /// The derivations this batch flipped to walked, by hash.
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
        }

        if hashes.is_empty() {
            return Ok(HashSet::new());
        }

        let rows = self
            .db()
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                WALKED_UPSERT,
                [
                    ids.into(),
                    hashes.into(),
                    names.into(),
                    architectures.into(),
                    pnames.into(),
                    prefer_local.into(),
                    fixed_output.into(),
                    allow_substitutes.into(),
                    Value::ChronoDateTime(Some(gradient_types::now())),
                ],
            ))
            .await
            .context("upsert walked derivations")?;

        Ok(rows
            .into_iter()
            .filter_map(|r| r.try_get::<String>("", "hash").ok())
            .collect())
    }

    /// A stub for every dependency the batch names and does not itself carry.
    /// An unparseable dependency path fails the batch: the source would
    /// otherwise commit `walked = true` with an edge missing, and `walked`
    /// never regresses, so no later walk would repair it.
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
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                STUB_INSERT,
                [
                    ids.into(),
                    hashes.into(),
                    names.into(),
                    Value::ChronoDateTime(Some(gradient_types::now())),
                ],
            ))
            .await
            .context("insert dependency stubs")?;

        Ok(())
    }

    /// Every drv path the batch names, walked or stub, to the row's id. Read
    /// back from the table after the inserts, never from a local guess.
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
                .query_all_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Postgres,
                    RESOLVE_IDS,
                    [chunk.to_vec().into()],
                ))
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

    /// Outputs and edges of every derivation the batch reports, not only the
    /// ones it flipped to walked. Both inserts are conflict-guarded no-ops on a
    /// record already written, and re-asserting the full declared set on every
    /// walk is the only repair a graph that lost an edge ever gets.
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
                // Unlike an unparseable dependency path, this one may stay a fallback: an
                // unknown hash matches no `cached_path` and no upstream, so the derivation
                // is never pruned nor counted cached, and simply gets built.
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
                return Err(anyhow!("failed to insert derivation outputs: {e}"));
            }
        }

        if edge_from.is_empty() {
            return Ok(Vec::new());
        }

        let grew = self
            .db()
            .query_all_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                EDGE_INSERT,
                [edge_from.into(), edge_to.into()],
            ))
            .await
            .context("insert dependency edges")?;

        Ok(grew
            .iter()
            .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
            .map(DerivationId::new)
            .collect())
    }

    async fn set_anchor_limits(
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
            .execute_raw(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                ANCHOR_LIMITS_UPDATE,
                [ids.into(), timeouts.into(), silents.into()],
            ))
            .await
            .context("set anchor limits")?;

        Ok(())
    }

    /// Build-once anchors for every named derivation, `ON CONFLICT DO NOTHING`
    /// so an anchor from a prior evaluation is untouched, then this
    /// evaluation's `build_job` rows, then the idempotent substitution facts:
    /// `substitutable` is set for an upstream hit and never cleared here, and a
    /// derivation whole in our cache is `Substituted`.
    async fn resolve_anchors(
        &self,
        ids: &HashMap<String, DerivationId>,
        derivations: &[DiscoveredDerivation],
        batch: &IngestBatch,
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
        let upstream: HashSet<DerivationId> = batch
            .upstream_substitutable
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

        let anchors: Vec<ADerivationBuild> = all_ids
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
                    substitutable: upstream.contains(&drv_id),
                    substituted: status == BuildStatus::Substituted,
                    timeout_secs,
                    max_silent_secs,
                    created_at: now,
                    updated_at: now,
                    ..Default::default()
                }
                .into_active_model()
            })
            .collect();

        for chunk in anchors.chunks(BATCH_SIZE) {
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
                return Err(anyhow!("failed to upsert anchors: {e}"));
            }
        }
        self.set_anchor_limits(&limits).await?;

        let db = self.db();
        let anchor_by_drv: HashMap<DerivationId, DerivationBuildId> =
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
                anchor_by_drv.get(drv_id).map(|&anchor_id| {
                    MBuildJob {
                        id: gradient_types::ids::BuildJobId::now_v7(),
                        evaluation: self.evaluation_id,
                        derivation: *drv_id,
                        derivation_build: anchor_id,
                        score: 0.0,
                        score_breakdown: serde_json::json!({}),
                        created_at: now,
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
                return Err(anyhow!("failed to upsert build_job rows: {e}"));
            }
        }

        if !upstream.is_empty() {
            let upstream_ids: Vec<DerivationId> = upstream.iter().copied().collect();
            gradient_db::for_each_chunk(&upstream_ids, |chunk| async move {
                EDerivationBuild::update_many()
                    .col_expr(
                        CDerivationBuild::Substitutable,
                        sea_orm::sea_query::Expr::value(true),
                    )
                    .filter(CDerivationBuild::Derivation.is_in(chunk))
                    .filter(CDerivationBuild::Substitutable.eq(false))
                    .filter(CDerivationBuild::Status.is_not_in([
                        i32::from(BuildStatus::Completed),
                        i32::from(BuildStatus::Substituted),
                    ]))
                    .exec(db)
                    .await
            })
            .await
            .context("flag anchors substitutable from upstream")?;
        }

        if !truly.is_empty() {
            let truly_ids: Vec<DerivationId> = truly.iter().copied().collect();
            let changes = gradient_db::substitute_created_anchors(db, &truly_ids)
                .await
                .context("substitute created anchors")?;
            gradient_db::emit_transition_effects(self.ctx, &changes).await;
        }

        Ok(())
    }

    /// Move the readiness counters this batch changed, in one transaction under one
    /// ordered lock: the anchors whose substitution it established become
    /// fetchable and their dependents' counters drop, `unready_deps` is recounted
    /// from ground truth, and whatever now passes the gates is queued.
    ///
    /// ONE lock over the union of the two sets, never one per set: two ordered
    /// acquisitions in the same transaction are not monotone across each other, and
    /// that is how an ABBA cycle with a concurrent retire is built. The lock also
    /// widens the seed from the walked-and-grew set to the union, which is sound
    /// because the seed is an absolute recount over ground truth: for an anchor
    /// whose edges did not move it either agrees with the maintained value or the
    /// maintained value was wrong.
    ///
    /// The mark runs BEFORE the seed, and that order is load-bearing. The seed
    /// EVALUATES the fetchability predicate on each dependency instead of reading
    /// the column, so a dependency this batch is about to flip already counts as
    /// ready to it; letting the flip's ripple decrement afterwards would take the
    /// dependent one BELOW its true count, and a negative counter never satisfies
    /// `= 0` again. Seeding last makes the absolute write the final word for every
    /// row this batch names, and leaves the ripple exact for every row outside it.
    ///
    /// Promotion is offered the whole locked set rather than the seeded part: an
    /// anchor this batch just made substitutable passes the gates on its own
    /// account, and no dependent's flip would ever queue it. Nothing here retracts
    /// a substitution fact, because a batch only ever adds them, so that symmetric
    /// loss belongs to the demote and the retire.
    ///
    /// The un-promote after the seed is the OTHER symmetric loss, and it is not
    /// optional. The mark's ripple is a RELATIVE move over a base the seed has not
    /// corrected yet, so for a row whose edges grew this batch the base is
    /// stale-low: an anchor at `unready_deps = 1` that gains an edge to something
    /// unfetchable is taken to 0 by the ripple, reported ready, and queued, and only
    /// then does the seed put it back to 1. Measured on Postgres 18: the row commits
    /// `Queued` with `unready_deps = 1` and dispatches against an input that is not
    /// in the cache. `promote` after the seed covers the row the seed brings DOWN to
    /// zero; nothing but this covers the row it raises.
    ///
    /// The transitions are collapsed for the same reason: such a row accumulates
    /// `Created` to `Queued` and `Queued` to `Created` in one transaction, and only
    /// the net move committed, so only the net move may fan out.
    async fn advance_readiness(
        &self,
        batch: &IngestBatch,
        resolved: &Resolved,
        newly_walked: &HashSet<String>,
        grew: Vec<DerivationId>,
    ) -> Result<()> {
        let mut to_seed: Vec<DerivationId> = newly_walked
            .iter()
            .filter_map(|h| resolved.by_hash.get(h).copied())
            .collect();
        to_seed.extend(grew);
        to_seed.sort_unstable();
        to_seed.dedup();

        let mut locked = to_seed;
        locked.extend(
            batch
                .truly_substituted
                .iter()
                .chain(batch.upstream_substitutable.iter())
                .filter_map(|p| resolved.by_path.get(p).copied()),
        );
        locked.sort_unstable();
        locked.dedup();
        if locked.is_empty() {
            return Ok(());
        }

        let txn = self
            .db()
            .begin()
            .await
            .context("begin the readiness transaction")?;
        let lock = gradient_db::lock_anchors(&txn, &locked).await?;
        let mut changes = gradient_db::became_fetchable(&lock)
            .await
            .context("advance fetchable anchors")?;
        gradient_db::seed_unready_deps(&lock)
            .await
            .context("seed unready_deps")?;
        changes.extend(
            gradient_db::promote(&txn, &locked)
                .await
                .context("promote the batch's anchors")?,
        );
        changes.extend(
            gradient_db::unpromote_ungated(&txn, &locked)
                .await
                .context("settle the queue against the seeded counts")?,
        );
        txn.commit()
            .await
            .context("commit the readiness transaction")?;
        let net = gradient_db::collapse_transitions(changes);
        gradient_db::emit_transition_effects(self.ctx, &net).await;

        Ok(())
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

pub(crate) async fn apply_batch(ctx: &DbContext, batch: &IngestBatch) -> Result<IngestReport> {
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
    let mut report = IngestReport {
        evaluation: evaluation_id,
        task: batch.task,
        ..Default::default()
    };
    if !batch.derivations.is_empty() {
        let newly_walked = writer.upsert_walked(&batch.derivations).await?;
        writer.insert_stubs(&batch.derivations).await?;
        let resolved = writer.resolve_ids(&batch.derivations).await?;
        let ids = &resolved.by_path;
        let grew = writer.insert_records(&batch.derivations, ids).await?;
        writer.persist_input_sources(&batch.derivations, ids).await;
        writer.persist_upstream_hits(&batch.upstream_hits).await;
        writer
            .resolve_anchors(ids, &batch.derivations, batch)
            .await?;
        writer.add_system_features(&batch.derivations, ids).await;
        writer
            .advance_readiness(batch, &resolved, &newly_walked, grew)
            .await?;
        report.entry_points = match batch.task {
            Some(task) => {
                writer
                    .process_entry_points(task, &batch.derivations, ids)
                    .await
            }
            None => Vec::new(),
        };
        report.walked = newly_walked.len();
        debug!(%evaluation_id, walked = report.walked, named = ids.len(), "batch written");
    }

    writer
        .record_eval_messages(&batch.warnings, &batch.errors)
        .await;
    Ok(report)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use gradient_entity::evaluation::EvaluationStatus;
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
    use std::collections::BTreeMap;

    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-a.drv";
    const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-b.drv";

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

    fn anchor_row(derivation: DerivationId) -> MDerivationBuild {
        MDerivationBuild {
            id: DerivationBuildId::now_v7(),
            derivation,
            ..Default::default()
        }
    }

    fn hash_row(hash: &str) -> BTreeMap<String, Value> {
        BTreeMap::from([("hash".to_owned(), Value::from(hash.to_owned()))])
    }

    fn drv_row(derivation: DerivationId) -> BTreeMap<String, Value> {
        BTreeMap::from([(
            "derivation".to_owned(),
            Value::from(derivation.into_inner()),
        )])
    }

    fn ripple_row(derivation: DerivationId, ready: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "derivation".to_owned(),
                Value::from(derivation.into_inner()),
            ),
            ("ready".to_owned(), Value::from(ready)),
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

    fn ok(n: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected: n,
        }
    }

    /// The rows behind the query script `apply_batch` replays for one walked
    /// derivation `a` that names `b`: the evaluation, the walked upsert's
    /// RETURNING, the id resolve, the anchor re-select. Every `insert_many`
    /// reads its primary key back, so those take an empty result set.
    fn scripted(evaluation: EvaluationId) -> (MEvaluation, MDerivation, MDerivation) {
        let eval = MEvaluation {
            id: evaluation,
            status: EvaluationStatus::EvaluatingDerivation,
            ..Default::default()
        };
        (eval, derivation_row(A, true), derivation_row(B, false))
    }

    /// A dependency the batch only names gets a stub row before the edge that
    /// points at it, never a walked one, and the batch reports one walked
    /// derivation: the one whose record it carried. The readiness pass closes the
    /// batch: it seeds from edges that exist, so it can only run once they do.
    #[tokio::test]
    async fn a_named_dependency_gets_a_stub_before_its_edge() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 3])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let report = apply_batch(
            &ctx,
            &IngestBatch {
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
            13,
            "evaluation, walked, stubs, resolve, edges, anchor insert, anchor select, jobs, lock, mark, seed, promote, unpromote: {log:?}"
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
            .position(|s| s.contains("ORDER BY derivation FOR UPDATE"))
            .expect("the readiness pass locks its anchors");
        let mark = log
            .iter()
            .position(|s| s.contains("SET fetchable = true"))
            .expect("the mark runs");
        let seed = log
            .iter()
            .position(|s| s.contains("SET unready_deps = (SELECT count(*)"))
            .expect("the seed runs");
        assert!(
            walked < stub && stub < edge && edge < lock,
            "walked, then stubs, then edges, and only then the counters: {log:?}"
        );
        assert!(
            lock < mark && mark < seed,
            "the lock precedes the flip, and the absolute seed is the last word on the batch's own rows: {log:?}"
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

    /// The readiness pass has one legal order and the order IS the correctness
    /// argument, so it is asserted literally: mark, ripple, promote, seed, promote,
    /// un-promote.
    ///
    /// Mark before seed because the seed evaluates the fetchability predicate and
    /// would otherwise let the ripple decrement a dependency it already counted as
    /// ready. Un-promote after the seed because the ripple's promote fires on a
    /// count the seed has not corrected yet: an anchor whose edge set grew this
    /// batch is taken to zero by a relative move over a stale-low base, queued, and
    /// only then raised again. Emitting the two halves of that bounce would announce
    /// a `Queued` that never committed, so the transitions are collapsed to the net
    /// move, which for this fixture is nothing.
    #[tokio::test]
    async fn the_readiness_pass_marks_ripples_promotes_seeds_then_settles_the_queue() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![drv_row(b.id)]])
            .append_query_results([vec![ripple_row(a.id, true)]])
            .append_query_results([vec![drv_row(a.id)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![transition_row(a.id, 1, 0)]])
            .append_exec_results(vec![ok(1); 3])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply_batch(
            &ctx,
            &IngestBatch {
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
        let ripple = at("unready_deps - c.n");
        let seed = at("SET unready_deps = (SELECT count(*)");
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

    /// A batch delivered a second time flips nothing: the walked upsert's
    /// `WHERE NOT derivation.walked` predicate returns no row, so `RETURNING`
    /// still means "this batch flipped it" and the report counts none. The
    /// batch does re-assert its records, which is the repair path for a lost
    /// edge, so every write it repeats has to be conflict-guarded.
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
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_exec_results(vec![ok(0); 1])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let report = apply_batch(
            &ctx,
            &IngestBatch {
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
            !log.iter().any(|s| s.contains("SET unready_deps = ")),
            "a batch that flipped nothing walked and grew no edge has no counter to move: {log:?}"
        );
    }

    /// A dependency path that is not a derivation path fails the batch instead
    /// of dropping the edge: the source would otherwise commit `walked = true`
    /// dependency-blind, and `walked` never regresses.
    #[tokio::test]
    async fn an_unparseable_dependency_fails_the_batch() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, _) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .into_connection();
        let (ctx, _pool) = ctx(db).await;

        let err = apply_batch(
            &ctx,
            &IngestBatch {
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

    /// A batch with no derivations writes nothing to the graph: the only
    /// statement is the evaluation lookup.
    #[tokio::test]
    async fn an_empty_batch_touches_only_the_evaluation() {
        let evaluation = EvaluationId::now_v7();
        let (eval, _, _) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let report = apply_batch(
            &ctx,
            &IngestBatch {
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

    /// An anchor inserted for a name arrives before the record that carries
    /// the derivation's limits, and the anchor insert lands on no row once it
    /// exists, so a walked record writes its limits by their own statement.
    #[tokio::test]
    async fn a_walked_record_writes_its_limits_onto_an_existing_anchor() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, _) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone()]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![anchor_row(a.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 3])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let mut record = drv(A, &[]);
        record.timeout_secs = Some(3600);
        apply_batch(
            &ctx,
            &IngestBatch {
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
        let anchors = log
            .iter()
            .position(|s| s.sql.contains("INSERT INTO \"derivation_build\""))
            .expect("the anchor insert runs");
        let limits = log
            .iter()
            .position(|s| s.sql.contains("UPDATE derivation_build AS db"))
            .expect("the limits update runs");
        let seed = log
            .iter()
            .position(|s| s.sql.contains("SET unready_deps = (SELECT count(*)"))
            .expect("the readiness seed runs");
        assert!(
            anchors < limits && limits < seed,
            "limits are written once the anchor exists, and the counters after both: {log:?}"
        );
        assert!(
            format!("{:?}", log[limits].values).contains("BigInt(Some(3600))"),
            "the update carries the record's limits: {:?}",
            log[limits]
        );
    }
}
