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
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel,
    QueryFilter, TransactionTrait, Value,
};
use tracing::{debug, error, warn};

use crate::messages::{IngestBatch, IngestReport, UpstreamHit};

const BATCH_SIZE: usize = 1000;

gradient_db::sql! {
    /// Insert or complete the record of every derivation the worker walked. The
    /// conflict update runs only for a row that is not yet walked, so RETURNING
    /// yields exactly the derivations this batch flipped. The record lands with
    /// its distinct input count as `unwalked_inputs`, so a non-leaf never reads
    /// complete between this write and the seed below it.
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

    /// A row for every dependency the batch names, so its edge can land now. A
    /// stub carries only what the path itself says; the walk fills the rest.
    STUB_INSERT = r#"
INSERT INTO derivation (id, hash, name, architecture, walked, created_at)
SELECT d.id, d.hash, d.name, '', false, $4
FROM unnest($1::uuid[], $2::text[], $3::text[]) AS d(id, hash, name)
ON CONFLICT (hash, name) DO NOTHING
"#,
        params = [NewUuids(64), DerivationHashes(64), Texts("hello", 64), Now];

    RESOLVE_IDS = "SELECT id, hash FROM derivation WHERE hash = ANY($1::text[])",
        params = [DerivationHashes(64)];

    /// `RETURNING` names exactly the dependents whose edge set GREW, so the readiness
    /// seed runs for those and not for every derivation the batch mentions.
    EDGE_INSERT = r#"
INSERT INTO derivation_dependency (derivation, dependency)
SELECT e.derivation, e.dependency FROM unnest($1::uuid[], $2::uuid[]) AS e(derivation, dependency)
ON CONFLICT DO NOTHING
RETURNING derivation
"#,
        params = [DerivationIds(64), DerivationIds(64)];

    /// A stub's anchor exists before the record that carries the derivation's
    /// limits, so they land here; `0` stands in for an unset limit in the arrays.
    ANCHOR_LIMITS_UPDATE = r#"
UPDATE derivation_build AS db
SET timeout_secs = NULLIF(l.timeout_secs, 0), max_silent_secs = NULLIF(l.max_silent_secs, 0)
FROM unnest($1::uuid[], $2::bigint[], $3::bigint[]) AS l(derivation, timeout_secs, max_silent_secs)
WHERE db.derivation = l.derivation
  AND (db.timeout_secs, db.max_silent_secs)
      IS DISTINCT FROM (NULLIF(l.timeout_secs, 0), NULLIF(l.max_silent_secs, 0))
"#,
        params = [DerivationIds(64), Ints(3600, 64), Ints(600, 64)];
}

gradient_db::sql_fn! {
    /// The exemplar behind [`flip_substitutable`]'s dynamic `NOT IN` fence: the
    /// gate plans against the same generated fragment the call site runs.
    FLIP_SUBSTITUTABLE = flip_substitutable_sql,
        params = [DerivationIds(64)];
}

fn flip_substitutable_sql() -> String {
    format!(
        "UPDATE derivation_build SET substitutable = true, \
         updated_at = (now() AT TIME ZONE 'UTC') \
         WHERE derivation = ANY($1::uuid[]) AND NOT substitutable \
           AND status NOT IN ({terminal_success}) \
         RETURNING derivation",
        terminal_success = gradient_db::status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS),
    )
}

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
            .query_all_raw(EDGE_INSERT.bind([edge_from.into(), edge_to.into()]))
            .await
            .context("insert dependency edges")?;

        // One row per landed edge, so a derivation with fifty new inputs is named
        // fifty times; every consumer wants the set.
        let mut grown: Vec<DerivationId> = grew
            .iter()
            .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
            .map(DerivationId::new)
            .collect();
        grown.sort_unstable();
        grown.dedup();

        Ok(grown)
    }

    /// The subtree bit the walk prunes on, settled on `derivation` rows before any
    /// anchor is locked: the class order is derivation first.
    ///
    /// The rows this batch flipped to walked are named as such: they were incomplete
    /// before it whatever they read now, since the upsert above wrote `walked` one
    /// statement ago, and a freshly walked leaf that reads complete on both sides of
    /// the seed still owes its dependents a count-down.
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

        let txn = self
            .db()
            .begin()
            .await
            .context("begin the walk completeness transaction")?;
        gradient_db::seed_walk_completeness(&txn, &walked, grew)
            .await
            .context("seed unwalked_inputs")?;
        txn.commit()
            .await
            .context("commit the walk completeness transaction")
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
            .execute_raw(ANCHOR_LIMITS_UPDATE.bind([ids.into(), timeouts.into(), silents.into()]))
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
                    // A batch claims nothing about upstreams any more: the probe
                    // runs once the anchor is demanded and flips this itself.
                    substitutable: false,
                    substituted: status == BuildStatus::Substituted,
                    // `..Default::default()` sends every column, so the database
                    // default never reaches a new row: this batch's recompute is
                    // what turns demand on for the anchors something reaches.
                    demanded: false,
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
    ///
    /// The demand this batch created and removed is settled afterwards, on the
    /// pooled handle: it writes rows no ordered lock names (the direct inputs of
    /// every new builder), and both statements re-check the gate, so it is a
    /// re-gate rather than a counter move and does not belong inside the counters'
    /// transaction.
    async fn advance_readiness(
        &self,
        batch: &IngestBatch,
        resolved: &Resolved,
        newly_walked: &HashSet<String>,
        grew: &[DerivationId],
        entry_points: &[DerivationId],
    ) -> Result<Vec<DerivationId>> {
        let mut to_seed: Vec<DerivationId> = newly_walked
            .iter()
            .filter_map(|h| resolved.by_hash.get(h).copied())
            .collect();
        to_seed.extend_from_slice(grew);
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
        self.move_batch_demand(&to_seed, entry_points).await
    }

    /// Settle the demand a batch moves without moving any anchor's status, so the
    /// transition emitter cannot see it: a newly walked or newly grown builder wants
    /// its inputs, an entry point wants its own derivation, and an anchor an upstream
    /// just claimed stops wanting anything below it. All three are the same event, an
    /// anchor whose demand changed, so all three are one recompute.
    async fn move_batch_demand(
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
        let mut gained_demand = Vec::new();
        for chunk in roots.chunks(gradient_db::IN_CHUNK_SIZE) {
            let moved = gradient_db::recompute_demand(db, chunk)
                .await
                .context("recompute what this batch demands")?;
            gained_demand.extend_from_slice(&moved.gained);
            for gained in moved.gained.chunks(gradient_db::IN_CHUNK_SIZE) {
                changes.extend(
                    gradient_db::promote(db, gained)
                        .await
                        .context("promote what this batch demands")?,
                );
            }
            for lost in moved.lost.chunks(gradient_db::IN_CHUNK_SIZE) {
                changes.extend(
                    gradient_db::unpromote_ungated(db, lost)
                        .await
                        .context("release undemanded relays")?,
                );
            }
        }
        gradient_db::emit_transition_effects(self.ctx, &changes).await;

        Ok(gained_demand)
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
        writer
            .record_walk_completeness(&resolved, &newly_walked, &grew)
            .await?;
        writer.persist_input_sources(&batch.derivations, ids).await;
        writer
            .resolve_anchors(ids, &batch.derivations, batch)
            .await?;
        writer.add_system_features(&batch.derivations, ids).await;
        // Entry points are demand, so their rows exist before the gates are read.
        report.entry_points = match batch.task {
            Some(task) => {
                writer
                    .process_entry_points(task, &batch.derivations, ids)
                    .await
            }
            None => Vec::new(),
        };
        report.gained_demand = writer
            .advance_readiness(batch, &resolved, &newly_walked, &grew, &report.entry_points)
            .await?;

        gradient_db::bump_graph_version(writer.db(), &[evaluation_id])
            .await
            .context("bump the graph version for the batch")?;

        // Edges are global: a derivation an older evaluation kept as a stub gains
        // a closure here, so that evaluation's histogram is stale too.
        if !grew.is_empty() {
            gradient_db::bump_graph_version_for_derivations(writer.db(), &grew)
                .await
                .context("bump the graph version of evaluations sharing the new edges")?;
        }

        report.walked = newly_walked.len();
        debug!(%evaluation_id, walked = report.walked, named = ids.len(), "batch written");
    }

    writer
        .record_eval_messages(&batch.warnings, &batch.errors)
        .await;
    Ok(report)
}

/// Set `substitutable` on the anchors an upstream now serves, returning the ones
/// that were not already flagged. Those stop being builders, so whatever they
/// listed as an input has just lost a demander, and nothing about that is a status
/// transition the emitter could notice.
///
/// A terminal-success anchor is left alone: its outputs are already ours, and
/// flipping it would send it back through a relay for bytes we hold.
async fn flip_substitutable<C: ConnectionTrait>(
    db: &C,
    upstream: &[DerivationId],
) -> Result<Vec<DerivationId>> {
    if upstream.is_empty() {
        return Ok(Vec::new());
    }

    let ids: Vec<uuid::Uuid> = upstream.iter().map(|d| d.into_inner()).collect();
    let rows = db
        .query_all_raw(FLIP_SUBSTITUTABLE.bind([ids.into()]))
        .await
        .context("flag anchors substitutable from upstream")?;

    Ok(rows
        .iter()
        .filter_map(|r| r.try_get::<uuid::Uuid>("", "derivation").ok())
        .map(DerivationId::new)
        .collect())
}

/// Apply what the upstream probe found: the narinfo onto every `derivation_output`
/// sharing the hash, the runtime edges its references name, `substitutable` on the
/// anchors whose every output is served, the wholeness those new edges change, and
/// the demand all of that moves.
///
/// This is the round: the demand this recompute turns on is handed back to the
/// probe by the transition emitter, so the next level of the closure is asked for
/// next. Nothing here reaches the network - the probe already ran.
pub(crate) async fn apply_upstream_hits(
    ctx: &DbContext,
    hits: &HashMap<String, UpstreamHit>,
) -> Result<()> {
    if hits.is_empty() {
        return Ok(());
    }

    let db = &ctx.worker_db;
    let txn = db.as_transaction().context(
        "UpstreamHits must run inside a transaction: the wholeness seed takes anchor locks",
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
        return Ok(());
    }

    persist_narinfo(db, &hit_rows, hits).await?;

    // A hit is only a relay when EVERY output of the anchor is served: an output
    // whose bytes nobody has would otherwise fail the substitution and escalate
    // into a build whose inputs were never produced.
    let outputs = gradient_db::fetch_in_chunks(&touched, |chunk| async move {
        EDerivationOutput::find()
            .filter(CDerivationOutput::Derivation.is_in(chunk))
            .all(db)
            .await
    })
    .await
    .context("load every output of the anchors an upstream hit names")?;
    let mut by_anchor: HashMap<DerivationId, Vec<bool>> = HashMap::new();
    for o in &outputs {
        by_anchor
            .entry(o.derivation)
            .or_default()
            .push(hits.contains_key(&o.hash) || o.is_cached_anywhere());
    }

    let served: Vec<DerivationId> = touched
        .iter()
        .copied()
        .filter(|d| {
            by_anchor
                .get(d)
                .is_some_and(|outs| !outs.is_empty() && outs.iter().all(|served| *served))
        })
        .collect();
    let newly_substitutable = flip_substitutable(db, &served).await?;

    let seeded = gradient_db::seed_runtime_deps(txn, &[], &touched).await?;
    if !seeded.whole.is_empty() {
        let lock = gradient_db::lock_anchors(txn, &seeded.whole).await?;
        let changes = gradient_db::became_fetchable(&lock).await?;
        gradient_db::emit_transition_effects(ctx, &changes).await;
    }

    let mut roots = touched;
    roots.extend_from_slice(&newly_substitutable);
    roots.sort_unstable();
    roots.dedup();

    let mut changes = Vec::new();
    for chunk in roots.chunks(gradient_db::IN_CHUNK_SIZE) {
        let moved = gradient_db::recompute_demand(db, chunk)
            .await
            .context("recompute what an upstream hit demands")?;
        changes.extend(gradient_db::promote(db, &moved.gained).await?);
        changes.extend(gradient_db::unpromote_ungated(db, &moved.lost).await?);
    }
    gradient_db::emit_transition_effects(ctx, &changes).await;

    Ok(())
}

/// Persist each hit onto the outputs sharing its hash and write the runtime edges
/// its `References:` line names. An output already cached anywhere is left alone:
/// what we hold beats what an upstream offers.
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

    // A narinfo is the other place runtime references are learned, so the edges
    // they name are written from the hit that carried them.
    for (derivation, tokens) in &learned {
        let producers = gradient_db::producers_of_tokens(db, tokens).await?;
        gradient_db::insert_runtime_edges(db, *derivation, &producers).await?;
    }

    Ok(())
}

/// What a landed batch triggers outside its transaction: forge checks for the
/// entry points, the per-task evaluation GC, and the live-channel ping.
pub(crate) async fn after_commit(
    ctx: &DbContext,
    actor: &ractor::ActorRef<crate::actor::GraphMsg>,
    batch: &IngestBatch,
    report: &IngestReport,
) {
    if report.skipped {
        return;
    }

    if let Some(task_id) = batch.task {
        gradient_db::announce_entry_point_statuses(ctx, report.evaluation, &report.entry_points)
            .await;
        if let Ok(Some(task)) = ETask::find_by_id(task_id).one(&ctx.worker_db).await {
            let gc_ctx = ctx.detached();
            let keep = task.keep_evaluations as usize;
            let actor = actor.clone();
            ctx.shutdown.spawn(async move {
                if let Err(e) = crate::gc::gc_task_evaluations(&gc_ctx, actor, task_id, keep).await
                {
                    error!(error = %e, %task_id, "GC: per-task evaluation GC failed");
                }
            });
        }
    }

    ctx.probe_requests.send(report.gained_demand.clone());

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
    use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Statement, Value};
    use std::collections::BTreeMap;

    /// A fresh evaluation's demand is established by the ingest walk, not by a
    /// status transition, so `emit_transition_effects` sees nothing left to gain
    /// and reports an empty set. The batch's own gained set is the only one the
    /// probe can learn from, and it is handed over after the commit: the probe
    /// reads the rows on its own connection, and an anchor it plans nothing for
    /// is still remembered as asked.
    #[tokio::test]
    async fn what_the_batch_demands_reaches_the_upstream_probe() {
        let gained = DerivationId::now_v7();
        let (ctx, _pool, mut probes) = crate::test_ctx::ctx_with_probes(
            MockDatabase::new(DatabaseBackend::Postgres).into_connection(),
        )
        .await;
        let actor = crate::Graph::new()
            .spawn(ctx.clone(), None, None)
            .await
            .expect("the graph actor starts");

        after_commit(
            &ctx,
            &actor,
            &IngestBatch {
                evaluation: EvaluationId::now_v7(),
                ..Default::default()
            },
            &IngestReport {
                gained_demand: vec![gained],
                ..Default::default()
            },
        )
        .await;

        assert_eq!(
            probes
                .try_recv()
                .expect("the batch's gained demand reaches the probe"),
            vec![gained],
        );
        actor.stop_and_wait(None, None).await.unwrap();
    }

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

    fn demand_row(derivation: DerivationId, demanded: bool) -> BTreeMap<String, Value> {
        BTreeMap::from([
            (
                "derivation".to_owned(),
                Value::from(derivation.into_inner()),
            ),
            ("demanded".to_owned(), Value::from(demanded)),
        ])
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
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
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
            19,
            "evaluation, walked, stubs, resolve, edges, walk lock, walk seed, anchor insert, anchor select, jobs, lock, mark, seed, promote, unpromote, version, and the raised, locked demand recompute: {log:?}"
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

    /// A narinfo names the references of a path we do not have yet, so the hit is
    /// the second place a runtime edge is learned: every reference with a producer
    /// becomes one from the output's own derivation, the anchor whose every output
    /// is served becomes a relay, and the demand all of that moves is recomputed
    /// before the reply. The probe's next round starts from what that turns on.
    #[tokio::test]
    async fn an_upstream_hit_writes_the_runtime_edges_its_narinfo_names() {
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
            "SET substitutable = true",
            "SET missing_runtime_deps = x.n",
            "region(evaluation, derivation, builder) AS",
        ] {
            assert!(
                log.iter().any(|s| s.contains(fragment)),
                "{fragment} never ran: {log:?}"
            );
        }
    }

    /// Ingest claims nothing about upstreams any more. A batch that flipped one on
    /// its own would relay an anchor nothing demands, which is the traffic lazy
    /// probing exists to stop.
    #[tokio::test]
    async fn a_batch_flips_nothing_substitutable_on_its_own() {
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
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply_batch(
            &ctx,
            &IngestBatch {
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
            !log.iter().any(|s| s.contains("SET substitutable = true")),
            "{log:?}"
        );
    }

    /// The subtree bit is settled on `derivation` rows, right after the edges land
    /// and before any anchor is locked: an abandoned walk then leaves its parents
    /// incomplete, and a concurrent walk that asks `prunable` in between never
    /// prunes at a parent whose inputs are still stubs.
    #[tokio::test]
    async fn a_walk_settles_the_subtree_bit_before_it_touches_an_anchor() {
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
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply_batch(
            &ctx,
            &IngestBatch {
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
            .position(|s| s.sql.contains("INSERT INTO derivation\n"))
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
        let anchor_lock = log
            .iter()
            .position(|s| s.sql.contains("FROM derivation_build") && s.sql.contains("FOR UPDATE"))
            .expect("the readiness pass locks its anchors");
        assert!(
            edges < seed && seed < anchor_lock,
            "edges, then the seed, then anchors: {log:?}"
        );
    }

    /// Edges are global, so a batch that grew one must bump every evaluation that
    /// already holds the derivation. The edge insert returns one row per landed
    /// edge, so a derivation with many new inputs is named many times; binding
    /// that raw would hand `= ANY($1)` tens of thousands of duplicate uuids on a
    /// first-delivery batch, which is what flips the planner off the `build_job`
    /// index. The set is what the bump wants.
    #[tokio::test]
    async fn the_cross_evaluation_bump_names_each_derivation_once() {
        let evaluation = EvaluationId::now_v7();
        let (eval, a, b) = scripted(evaluation);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![eval]])
            .append_query_results([vec![hash_row(&a.hash)]])
            .append_query_results([vec![a.clone(), b.clone()]])
            // the edge insert names the same derivation once per landed edge
            .append_query_results([vec![drv_row(a.id), drv_row(a.id)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            // stubs, lock, seed, the batch's own bump, the cross-evaluation bump,
            // then the demand recompute's raise and lock
            .append_exec_results(vec![ok(1); 7])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply_batch(
            &ctx,
            &IngestBatch {
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
                    .contains("SELECT evaluation FROM build_job WHERE derivation = ANY")
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
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![drv_row(b.id)]])
            .append_query_results([vec![ripple_row(a.id, true)]])
            .append_query_results([vec![drv_row(a.id)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![transition_row(a.id, 1, 0)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
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

    /// A newly walked builder wants its whole pending closure in our cache, and
    /// nothing about that is a status transition the effects emitter could notice:
    /// the anchors already exist, at the status they already had. So the batch
    /// recomputes demand below its own roots after its counters have committed, and
    /// promotes what gained it with the ordinary gated statement (the candidate list
    /// is a bound, never a claim).
    #[tokio::test]
    async fn a_batch_promotes_what_its_new_builders_demand() {
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
            .append_query_results([vec![anchor_row(a.id), anchor_row(b.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            // what the recompute walk found and then wrote, then the relay it queues
            .append_query_results([vec![demand_row(b.id, true)], vec![demand_row(b.id, true)]])
            .append_query_results([vec![drv_row(b.id)]])
            .append_exec_results(vec![ok(1); 6])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        apply_batch(
            &ctx,
            &IngestBatch {
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
            .position(|s| s.contains("FROM region r ORDER BY r.derivation"))
            .expect("the batch recomputes what it demands");
        let demand = log
            .iter()
            .position(|s| s.contains("SET demanded ="))
            .expect("the batch writes what the recompute answered");
        let seed = log
            .iter()
            .position(|s| s.contains("SET unready_deps = (SELECT count(*)"))
            .expect("the seed runs");
        let promote = log
            .iter()
            .enumerate()
            .filter(|(_, s)| s.contains("queued_at = coalesce(db.queued_at"))
            .map(|(i, _)| i)
            .next_back()
            .expect("the demanded relay is promoted");

        assert!(
            seed < walk && walk < demand && demand < promote,
            "demand settles after the counters, and its promote reads the settled gate: {log:?}"
        );
        assert!(
            log[walk].contains("demanded(derivation) AS"),
            "the recompute is the closure walk, not one hop: {log:?}"
        );
        assert!(
            log[promote].contains("AND db.demanded"),
            "the promote reads the column the recompute just wrote: {log:?}"
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
            .append_exec_results(vec![ok(0); 2])
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
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![completeness_row(a.id, false, false)]])
            .append_query_results([Vec::<MDerivationBuild>::new()])
            .append_query_results([vec![anchor_row(a.id)]])
            .append_query_results([Vec::<MBuildJob>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![ok(1); 6])
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
