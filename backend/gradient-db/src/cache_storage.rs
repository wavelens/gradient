/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Storage-accounting helpers for the per-cache / per-instance max-storage
//! gate. Usage is the logical sum of `cached_path.file_size` (compressed NAR
//! bytes): per-cache via the `cached_path_signature` join, instance-wide as a
//! global sum. Backend-agnostic (works for local FS and S3).

use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::cache::Model as MCache;
use gradient_entity::cached_path::{Column as CCachedPath, Entity as ECachedPath};
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_types::ids::{CacheId, DerivationBuildId, DerivationId, ProjectId};
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect, Select,
};
use tracing::warn;

/// Park threshold: a cache with less than this much free headroom is "full".
pub const STORAGE_HEADROOM_BYTES: i64 = 10 * 1024 * 1024;

/// `SUM(file_size)` cast back to `BIGINT`: Postgres widens `SUM(int8)` to
/// `NUMERIC`, which would otherwise fail to decode into `Option<i64>`.
fn file_size_sum_bigint() -> Expr {
    use gradient_entity::cached_path::Column as CCP;
    use sea_orm::sea_query::ExprTrait;
    CCP::FileSize.sum().cast_as(Alias::new("bigint"))
}

const BYTES_PER_GB: i64 = 1024 * 1024 * 1024;

fn limit_to_bytes(max_storage_gb: i32) -> Option<i64> {
    if max_storage_gb <= 0 {
        None
    } else {
        Some(max_storage_gb as i64 * BYTES_PER_GB)
    }
}

/// Sum of compressed NAR bytes attributed to a single cache.
pub async fn cache_used_bytes<C: ConnectionTrait>(
    db: &C,
    cache: CacheId,
) -> Result<i64, sea_orm::DbErr> {
    use gradient_entity::cached_path::{Column as CCP, Entity as ECP};
    use gradient_entity::cached_path_signature::{Column as CSig, Entity as ESig};

    let path_ids: Vec<gradient_entity::ids::CachedPathId> = ESig::find()
        .filter(CSig::Cache.eq(cache))
        .all(db)
        .await?
        .into_iter()
        .map(|s| s.cached_path)
        .collect();

    if path_ids.is_empty() {
        return Ok(0);
    }

    let mut total: i64 = 0;
    for chunk in path_ids.chunks(crate::IN_CHUNK_SIZE) {
        let sum: Option<i64> = ECP::find()
            .filter(CCP::Id.is_in(chunk.to_vec()))
            .select_only()
            .column_as(file_size_sum_bigint(), "total")
            .into_tuple()
            .one(db)
            .await?
            .flatten();
        total += sum.unwrap_or(0);
    }
    Ok(total)
}

/// Sum of compressed NAR bytes stored across the whole instance.
pub async fn instance_used_bytes<C: ConnectionTrait>(db: &C) -> Result<i64, sea_orm::DbErr> {
    use gradient_entity::cached_path::Entity as ECP;
    let sum: Option<i64> = ECP::find()
        .select_only()
        .column_as(file_size_sum_bigint(), "total")
        .into_tuple()
        .one(db)
        .await?
        .flatten();
    Ok(sum.unwrap_or(0))
}

/// The active, writable (ReadWrite/WriteOnly) caches a project can push to.
pub async fn project_writable_caches<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MCache>, sea_orm::DbErr> {
    use gradient_entity::cache::{Column as CCache, Entity as ECache};
    use gradient_entity::project_cache::{Column as COC, Entity as EOC};

    let cache_ids: Vec<CacheId> = EOC::find()
        .filter(COC::Project.eq(project))
        .filter(COC::Mode.is_in([
            CacheSubscriptionMode::ReadWrite,
            CacheSubscriptionMode::WriteOnly,
        ]))
        .all(db)
        .await?
        .into_iter()
        .map(|r| r.cache)
        .collect();

    if cache_ids.is_empty() {
        return Ok(Vec::new());
    }

    ECache::find()
        .filter(CCache::Id.is_in(cache_ids))
        .filter(CCache::Active.eq(true))
        .all(db)
        .await
}

/// Free headroom (bytes) for one cache, bounded by both its own limit and the
/// instance-wide limit. A non-positive limit means unlimited on that axis.
/// Returns `i64::MAX` when both axes are unlimited.
fn headroom(
    cache_limit_gb: i32,
    cache_used: i64,
    instance_limit_gb: i32,
    instance_used: i64,
) -> i64 {
    let cache_free = limit_to_bytes(cache_limit_gb)
        .map(|lim| lim - cache_used)
        .unwrap_or(i64::MAX);
    let instance_free = limit_to_bytes(instance_limit_gb)
        .map(|lim| lim - instance_used)
        .unwrap_or(i64::MAX);
    cache_free.min(instance_free)
}

/// `true` when the project has at least one writable cache AND every writable cache
/// has less than `STORAGE_HEADROOM_BYTES` free. An empty writable-cache set
/// returns `false` (that case is owned by the NoCache gate).
pub async fn project_caches_all_full<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    instance_limit_gb: i32,
) -> Result<bool, sea_orm::DbErr> {
    let caches = project_writable_caches(db, project).await?;
    if caches.is_empty() {
        return Ok(false);
    }
    let instance_used = instance_used_bytes(db).await?;
    for cache in &caches {
        let used = cache_used_bytes(db, cache.id).await?;
        let free = headroom(cache.max_storage_gb, used, instance_limit_gb, instance_used);
        if free >= STORAGE_HEADROOM_BYTES {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Why an input the worker reported missing was nonetheless treated as
/// available. Captured by the scheduler's missing-input self-heal so the cause
/// (stale `cached_path` whose object was GC'd or never uploaded, vs a producer
/// trusted `Substituted`/`Completed` while its NAR was never cached) is visible
/// at `warn` without raising the global log level.
#[derive(Debug, Default)]
pub struct MissingInputDiagnosis {
    /// A `cached_path` row exists for this output hash.
    pub cached_path_present: bool,
    /// That row claims a fully-uploaded NAR (`file_hash IS NOT NULL`).
    pub fully_cached: bool,
    /// `derivation_output` rows for this hash, and how many are `is_cached`.
    pub outputs_total: usize,
    pub outputs_cached: usize,
    /// Statuses of builds in this evaluation that produce the output.
    pub producer_build_statuses: Vec<gradient_entity::build::BuildStatus>,
}

/// Snapshot the cache/build state of a missing input `hash` within an
/// evaluation, for diagnostic logging by the missing-input self-heal.
pub async fn diagnose_missing_input<C: ConnectionTrait>(
    db: &C,
    _evaluation_id: gradient_types::ids::EvaluationId,
    hash: &str,
) -> Result<MissingInputDiagnosis, sea_orm::DbErr> {
    use gradient_entity::cached_path::{Column as CCP, Entity as ECP};
    use gradient_entity::derivation_build::{Column as CDB, Entity as EDB};
    use gradient_entity::derivation_output::{Column as CDO, Entity as EDO};

    let cached_path = ECP::find().filter(CCP::Hash.eq(hash)).one(db).await?;
    let outputs = EDO::find().filter(CDO::Hash.eq(hash)).all(db).await?;
    let outputs_cached = outputs.iter().filter(|o| o.is_cached).count();
    let producer_drvs: Vec<DerivationId> = outputs.iter().map(|o| o.derivation).collect();

    // Anchors are global; the producer's build status is the same regardless of
    // the querying evaluation.
    let producer_build_statuses = if producer_drvs.is_empty() {
        Vec::new()
    } else {
        EDB::find()
            .filter(CDB::Derivation.is_in(producer_drvs))
            .all(db)
            .await?
            .into_iter()
            .map(|b| b.status)
            .collect()
    };

    Ok(MissingInputDiagnosis {
        cached_path_present: cached_path.is_some(),
        fully_cached: cached_path.map(|c| c.is_fully_cached()).unwrap_or(false),
        outputs_total: outputs.len(),
        outputs_cached,
        producer_build_statuses,
    })
}

/// Reset a `derivation_output` to "present on no cache": clears the our-cache
/// link (`is_cached`/`cached_path`) **and** the upstream-availability record
/// (`external_url` + narinfo metadata). Clearing `external_url` is what makes
/// `is_cached_anywhere()` false, so nothing trusts an upstream offer for a copy
/// that is gone; leaving it set would record that offer against a node reset to a
/// real build - an unbuildable `InputsUnavailable` dead-end. Whether the eval walks
/// the node again is `walked`'s decision alone ([`crate::unwalk_derivations`]).
fn demoted_output(
    o: gradient_entity::derivation_output::Model,
) -> gradient_entity::derivation_output::ActiveModel {
    use sea_orm::{ActiveValue::Set, IntoActiveModel};

    let mut active = o.into_active_model();
    active.is_cached = Set(false);
    active.cached_path = Set(None);
    active.external_url = Set(None);
    active.nar_hash = Set(None);
    active.file_hash = Set(None);
    active.file_size = Set(None);
    active.references = Set(None);
    active.deriver = Set(None);
    active
}

/// Whether `demote_cached_output` must PRESERVE a reported-missing artifact
/// instead of deleting it: a producerless input (source / `.drv`) whose NAR is
/// still present. Nothing rebuilds such an input, so deleting the only copy
/// dead-ends every dependent on `InputsUnavailable`; a present object means the
/// failure was transient. An output (`has_producer`, restored by its rebuild) or
/// a genuinely-absent object is never preserved.
fn preserve_missing_artifact(has_producer: bool, object_present: bool) -> bool {
    !has_producer && object_present
}

crate::sql! {
    CLEAR_SUBSTITUTABLE_TRUST = "UPDATE derivation_build SET substitutable = false \
             WHERE derivation = ANY($1) AND substitutable",
        params = [DerivationIds(64)];
}

/// Purge a cached output proven unfetchable, so the next evaluation rebuilds it
/// from scratch as if it had never been cached. Clears `is_cached` /
/// `cached_path` on every `derivation_output` with this store-path `hash`,
/// retires the `cached_path` row itself
/// ([`crate::nar_closure::retire_paths`]: the row goes, its
/// `cached_path_signature` rows cascade, the `derivation_output` FK is
/// `ON DELETE SET NULL`, and every referrer's reference counter, gate flag and
/// anchor moves back), and removes the NAR object from storage so the row and the
/// object stay in step. The derivation
/// graph is left intact - only the cache artifact is removed. Returns the
/// producing derivations for logging. A producerless input (`.drv`/source) is
/// only purged when its NAR is genuinely gone: a still-present one is preserved
/// (see [`preserve_missing_artifact`]) since nothing else can restore it.
///
/// The producer reset lives in the retire, which decides it from the `fetchable`
/// flag it has just written rather than from this hash: what this does here is drop
/// the upstream trust the demoted output carried, so the anchor stops being a relay
/// against an artifact nothing serves.
///
/// That clear runs at every status, and it decides which arm of `gates_predicate`
/// the anchor takes, so a `Queued` relay whose demand was its only satisfier would
/// be left queued with its gates false - and dispatch reads the status rather than
/// the gates, so it would dispatch that anchor against a `.drv` nothing can serve.
/// The un-promote that follows is what closes that, and it shares the retire's
/// transaction so a crash cannot separate the two. The promote AFTER the commit is
/// the other half: an anchor that stops being a relay becomes a builder, at an
/// unchanged status the transition emitter cannot notice, and a builder demands its
/// direct inputs.
///
/// Because the clear has to run inside that transaction AND before the retire reads
/// `substitutable`, this is the one path that would write `derivation_build` before
/// touching `cached_path`. It opens with [`crate::nar_closure::lock_paths`] instead,
/// so the class order every other writer follows (`cached_path`, then
/// `derivation_build`) holds here too and a concurrent TTL or zombie retire cannot
/// deadlock against it. The retire's own pass then re-acquires a row this
/// transaction already holds, which is free.
///
/// [`crate::readiness::lock_anchors`] follows it for the same reason one class down.
/// The clear names its producers in a single UPDATE, so it acquires them in plan
/// order, and a hash with several producers can then hold one while
/// [`crate::readiness::repair_pending`]'s ordered chunk holds another. Taking the
/// ordered pass first means every `derivation_build` row this transaction writes is
/// already held in `derivation` order, and the retire's own anchor pass re-acquires
/// them for free.
pub async fn demote_cached_output(
    ctx: &crate::DbContext,
    hash: &str,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    use gradient_entity::derivation_output::{Column as CDO, Entity as EDO};
    use sea_orm::{ActiveModelTrait, TransactionTrait};

    let db = &ctx.worker_db;
    let nar_storage = &ctx.storage.nar_storage;
    let outputs = EDO::find().filter(CDO::Hash.eq(hash)).all(db).await?;
    let mut producers = Vec::with_capacity(outputs.len());
    for o in outputs {
        producers.push(o.derivation);
        demoted_output(o).update(db).await?;
    }

    // A producerless input (a build-time source or a `.drv`) has nothing to
    // rebuild it, so deleting a still-present NAR destroys the only copy and
    // dead-ends every dependent on `InputsUnavailable` forever (a producer's
    // output, by contrast, is restored by the rebuild below). Only purge it when
    // the object is genuinely gone - a zombie the dispatch gate must stop trusting;
    // a present object means the fetch failure was transient, so keep it and let
    // the build retry. A probe error preserves (never destroy on uncertainty).
    let has_producer = !producers.is_empty();
    let object_present = !has_producer && nar_storage.exists(hash).await.unwrap_or(true);
    if preserve_missing_artifact(has_producer, object_present) {
        return Ok(producers);
    }

    // The artifact is gone, so the upstream offer recorded with it is not evidence
    // any more: `demoted_output` cleared `external_url`, and this clears the
    // anchor's `substitutable` so the retire's `fetchable` mark sees the truth. The
    // next eval re-marks it substitutable if it is genuinely still on an upstream.
    let txn = db.begin().await?;
    let _paths = crate::nar_closure::lock_paths(&txn, &[hash.to_owned()]).await?;
    let _anchors = crate::readiness::lock_anchors(&txn, &producers).await?;
    if !producers.is_empty() {
        let ids: Vec<uuid::Uuid> = producers.iter().map(|d| d.into_inner()).collect();
        txn.execute_raw(CLEAR_SUBSTITUTABLE_TRUST.bind([ids.into()]))
            .await?;
    }

    let mut retired = crate::nar_closure::retire_paths(&txn, &[hash.to_owned()]).await?;
    retired
        .transitions
        .extend(crate::readiness::unpromote_ungated(&txn, &producers).await?);
    txn.commit().await?;

    // Clearing `substitutable` turns these producers back into builders, so demand
    // reaches their whole pending closure again and not just one hop.
    let moved = crate::readiness::recompute_demand(db, &producers).await?;
    retired
        .transitions
        .extend(crate::readiness::promote(db, &moved.gained).await?);
    retired
        .transitions
        .extend(crate::readiness::unpromote_ungated(db, &moved.lost).await?);
    crate::status::emit_transition_effects(ctx, &retired.transitions).await;

    if let Err(e) = nar_storage.delete(hash).await {
        warn!(%hash, error = %e, "demote: failed to delete NAR object from storage");
    }

    Ok(producers)
}

/// Demote every cached **output** that directly references `missing_hash`, so its
/// producer rebuilds and re-pushes the missing path. Only rebuildable output
/// referrers are demoted ([`OUTPUT_REFERRERS_SELECT`]): a producerless referrer -
/// a `.drv` or an input source - is left in place. Deleting one re-pushes nothing
/// (no producer rebuilds) and would strand the `.drv`'s own live dependents behind
/// the `.drv`-importable term of [`crate::graph_sql::gates_predicate`], a permanent
/// dead zone, since a genuinely missing input `.drv`/source is re-supplied only by
/// a full re-eval. The transitive completeness invariant is handled by the reverse
/// ripple inside [`crate::nar_closure::retire_paths`], which raises the referrers'
/// counters and leaves their healthy NARs in place. Returns the producers reset to
/// `Created`.
pub async fn demote_referrers_of(
    ctx: &crate::DbContext,
    missing_hash: &str,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    let mut producers = Vec::new();
    for referrer_hash in output_referrers_of_hash(&ctx.worker_db, missing_hash).await? {
        producers.extend(demote_cached_output(ctx, &referrer_hash).await?);
    }

    Ok(producers)
}

crate::sql! {
    OUTPUT_ONLY_CACHED_DEP_HASHES = r#"
        SELECT DISTINCT o.hash
        FROM derivation_dependency e
        JOIN derivation_output o ON o.derivation = e.dependency
        JOIN cached_path cp ON cp.hash = o.hash AND cp.file_hash IS NOT NULL
        WHERE e.derivation = $1 AND o.external_url IS NULL
        "#,
        params = [DerivationId];
}

/// Demote every output-only-cached **direct build dependency** of `derivation`
/// (output present in our cache, not on a real upstream). Recovers an *absent
/// orphan*: when a build fails on an input that has no producer row and no
/// reference-index referrer, the orphan was pruned out of the graph under one of
/// this build's cached deps - and being absent, it cannot be reached upward. So
/// reach it from the known failing build downward: demoting its output-only-cached
/// deps forces the next eval to re-walk them (`unwalk_derivations` drops their record
/// and closes their gates), re-record the dropped edges, and schedule the orphan.
/// Upstream-fetchable deps (`external_url`) are left intact - their closure is served
/// whole by the upstream. Returns producers reset to `Created`.
pub async fn demote_output_only_cached_deps(
    ctx: &crate::DbContext,
    derivation: DerivationId,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    use sea_orm::FromQueryResult;

    #[derive(sea_orm::FromQueryResult)]
    struct OutputHash {
        hash: String,
    }

    let db = &ctx.worker_db;
    let hashes = OutputHash::find_by_statement(
        OUTPUT_ONLY_CACHED_DEP_HASHES.bind([derivation.into_inner().into()]),
    )
    .all(db)
    .await?;

    let mut producers = Vec::new();
    for h in hashes {
        producers.extend(demote_cached_output(ctx, &h.hash).await?);
    }
    producers.sort_unstable();
    producers.dedup();

    // The demote alone no longer re-walks anything: the walk prunes on `walked`, and
    // the cache facts it used to read are exactly what a demote clears.
    let changes = crate::readiness::unwalk_derivations(ctx, &producers).await?;
    crate::status::emit_transition_effects(ctx, &changes).await;

    Ok(producers)
}

/// Enforce the row-vs-object invariant: **every** output of a terminal-success
/// producer (`Completed`/`Substituted`) must have a backing artifact - present in
/// our cache (`cached_path` with a NAR) or on a configured upstream
/// (`external_url`). Three ways the invariant breaks, all the same dead zone: the
/// cache GC deletes a `cached_path` row when its NAR object is gone (zombie purge,
/// stale-path eviction); an old global cache hit marks an anchor `Completed` + `is_cached`
/// without ever building it; or a partial cache-hit / substitution marks an anchor
/// `Completed` with an output that was never cached at all (`is_cached = false`, no
/// build attempt - observed on multi-output CUDA derivations whose `out` was never
/// pushed). Such an anchor's dependents are blocked at promotion (its
/// `fetchable` is - correctly - false, so it counts toward their `unready_deps`),
/// so they never dispatch, so no build
/// ever reports the path missing and the reactive `reconcile_missing_inputs` heal
/// never fires: a permanent dead zone. Demote each unbacked output - reset its
/// producer to `Created`, drop the stale flags, and raise the reference counters
/// of its referrers - so the next build rebuilds it. Returns the producers reset.
///
/// Keyed on the **ground truth** (a backing `cached_path` NAR), NOT the derived
/// `is_cached` flag: that flag is `false` for exactly the never-cached-output dead
/// zone above, so an `is_cached`-gated predicate would skip the anchors this sweep
/// exists to rescue. Nor the readiness flags, which are - correctly - already
/// closed for every dead-zone anchor. `external_url IS NULL` excludes outputs fetched
/// straight from an upstream (not from our cache). The completion path records each
/// output's `cached_path` before flipping the anchor terminal (#303/#399), so a
/// genuinely-complete anchor is never selected mid-completion.
///
/// This is the invariant itself, which is what the consistency report counts. The
/// heal acts on two disjoint halves of it: [`demotable_unbacked_outputs_select`]
/// (a rebuild is still worth trying) and [`fail_unhealable_anchors_sql`] (it is
/// not, and the producer is failed).
pub(crate) fn unbacked_trusted_outputs_select() -> String {
    format!(
        r#"
    SELECT DISTINCT o.hash
    FROM derivation_output o
    JOIN derivation_build db ON db.derivation = o.derivation
    WHERE db.status IN ({terminal_success})
      AND o.external_url IS NULL
      AND NOT EXISTS (
          SELECT 1 FROM cached_path cp
          WHERE cp.hash = o.hash AND cp.file_hash IS NOT NULL)
"#,
        terminal_success = crate::status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS),
    )
}

/// The invariant minus the anchors a rebuild has already been spent on (#654).
///
/// A demote is a bet that a rebuild lands the artifact, and it is placed once. Once
/// the fleet has finished a real build of the producer and the output is STILL
/// unbacked, the rebuild provably does not restore it, and demoting again only
/// re-queues the same build on the next pass - the `demote -> promote -> rebuild ->
/// demote` loop, one dispatch per reconcile pass, forever. A relay attempt is not
/// that evidence: it moves upstream bytes into the cache and says nothing about
/// what building the derivation lands, and the demote clears `substitutable` so the
/// retry it grants is a real build.
fn demotable_unbacked_outputs_select() -> String {
    format!(
        "{invariant}      AND NOT {built}\n",
        invariant = unbacked_trusted_outputs_select(),
        built = already_built("db", ""),
    )
}

/// SQL predicate: the fleet has finished a real build of the anchor aliased
/// `{alias}` - the builder ran, or its daemon found the outputs already valid.
/// Relay attempts are excluded: a relay never builds anything. `bound` is an extra
/// term on the attempt, for a caller that needs the build to have settled.
fn already_built(alias: &str, bound: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM build_attempt ba WHERE ba.derivation_build = {alias}.id \
         AND NOT ba.substitute AND ba.outcome IN ({success}){bound})",
        success = crate::status_sql::attempt_outcome_in(&AttemptOutcome::SUCCESS),
    )
}

/// The other half: fail the producer whose rebuild is spent and whose output never
/// came. The anchor is terminal-*success* against an artifact nothing has, so no
/// requeue path reaches it and `fetchable` can never hold - its dependents count it
/// unready forever. Leaving it is the dead zone this whole heal exists to close,
/// and demoting it again is the loop, so the third answer is the true one: the
/// build is a failure, recorded as one.
///
/// `$1` is the upload-grace cutoff. `JobCompleted` rides the control lane and
/// overtakes its own trailing `NarUploaded` commits, so an output is legitimately
/// unbacked for a while after its build reports success; `nar_upload_grace_hours`
/// is the bound this system already uses for exactly that window (the orphan-files
/// GC and the uploader's absent-row demote both measure against it). Only a build
/// that finished before it has run out of excuses.
fn fail_unhealable_anchors_sql() -> String {
    let unbacked = unbacked_output_of("db");
    format!(
        "UPDATE derivation_build db \
         SET status = {failed}, updated_at = (now() AT TIME ZONE 'UTC') \
         FROM derivation_build old \
         WHERE old.id = db.id \
           AND db.status IN ({terminal_success}) \
           AND {built} \
           AND EXISTS ({unbacked}) \
         RETURNING db.derivation, db.id AS anchor, old.status AS from_status, \
                   db.status AS to_status, ({unbacked} ORDER BY o.hash LIMIT 1) AS missing",
        failed = crate::status_sql::build(BuildStatus::FailedPermanent),
        terminal_success = crate::status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS),
        built = already_built("db", " AND ba.build_finished_at < $1"),
    )
}

/// An output of the anchor aliased `{alias}` that neither our cache nor an upstream
/// has. Written once and used twice: as the UPDATE's predicate, and to name the
/// output in the failure the operator reads.
fn unbacked_output_of(alias: &str) -> String {
    format!(
        "SELECT o.hash FROM derivation_output o \
         WHERE o.derivation = {alias}.derivation AND o.external_url IS NULL \
           AND NOT EXISTS (SELECT 1 FROM cached_path cp \
                           WHERE cp.hash = o.hash AND cp.file_hash IS NOT NULL)"
    )
}

crate::sql_fn! {
    UNBACKED_TRUSTED_OUTPUTS = unbacked_trusted_outputs_select,
        params = [],
        tier = Sweep;

    DEMOTABLE_UNBACKED_OUTPUTS = demotable_unbacked_outputs_select,
        params = [],
        tier = Sweep;

    FAIL_UNHEALABLE_ANCHORS = fail_unhealable_anchors_sql,
        params = [Now],
        tier = Sweep;
}

/// What one pass of the unbacked-output heal did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UnbackedSweep {
    /// Producers reset to `Created`, so the rebuild they are owed can restore
    /// their output.
    pub demoted: u64,
    /// Producers failed `FailedPermanent`: their rebuild is spent and the output
    /// never came.
    pub failed: u64,
}

pub async fn demote_unbacked_trusted_outputs(
    ctx: &crate::DbContext,
) -> Result<UnbackedSweep, sea_orm::DbErr> {
    use sea_orm::FromQueryResult;

    #[derive(sea_orm::FromQueryResult)]
    struct OutputHash {
        hash: String,
    }

    let db = &ctx.worker_db;
    let mut report = UnbackedSweep::default();

    for output in OutputHash::find_by_statement(DEMOTABLE_UNBACKED_OUTPUTS.stmt())
        .all(db)
        .await?
    {
        report.demoted += demote_cached_output(ctx, &output.hash).await?.len() as u64;
    }

    report.failed = fail_unhealable_anchors(ctx).await?;

    Ok(report)
}

/// Fail every producer whose granted rebuild came and went without backing its
/// output, and record why on the attempt that reported success - the answer an
/// attempt gives is "did this deliver the outputs", and at `JobCompleted` that
/// answer is optimistic. [`AttemptFailureReason::OutputMissing`] is deterministic,
/// so the next evaluation does not thaw the anchor into a rebuild that reproduces
/// it; the artifact appearing is what recovers it, through
/// `reconcile_cached_anchors_for_eval`.
/// The status and the reason share one transaction: a `FailedPermanent` anchor
/// whose attempt still reads `Built` is one the next evaluation thaws straight back
/// into the rebuild this verdict exists to stop.
async fn fail_unhealable_anchors(ctx: &crate::DbContext) -> Result<u64, sea_orm::DbErr> {
    use gradient_entity::build_attempt::{AttemptFailureReason, AttemptOutcome as Outcome};
    use sea_orm::TransactionTrait;

    let cutoff = gradient_types::now()
        - chrono::Duration::hours(ctx.config.storage.nar_upload_grace_hours.max(0));

    let txn = ctx.worker_db.begin().await?;
    let rows = txn
        .query_all_raw(FAIL_UNHEALABLE_ANCHORS.bind([sea_orm::Value::ChronoDateTime(Some(cutoff))]))
        .await?;

    let failed: Vec<(DerivationBuildId, String)> = rows
        .iter()
        .filter_map(|r| {
            Some((
                DerivationBuildId::new(r.try_get::<uuid::Uuid>("", "anchor").ok()?),
                r.try_get::<String>("", "missing").ok()?,
            ))
        })
        .collect();

    for (anchor, missing) in &failed {
        warn!(
            %anchor, %missing,
            "the build completed and the cache never got this output; failing the producer \
             instead of rebuilding it again"
        );
        crate::build_attempt::fail_latest_attempt(
            &txn,
            *anchor,
            Outcome::Failed,
            Some(AttemptFailureReason::OutputMissing),
            Some(format!(
                "the build completed but output {missing} never reached the cache; \
                 a rebuild reproduces this, so the producer is failed rather than re-queued"
            )),
        )
        .await?;
    }
    txn.commit().await?;

    if failed.is_empty() {
        return Ok(0);
    }

    let changes = crate::promotion::returned_transitions(rows);
    crate::status::emit_transition_effects(ctx, &changes).await;

    Ok(failed.len() as u64)
}

crate::sql! {
    OUTPUT_REFERRERS_SELECT = "SELECT DISTINCT r.referrer \
     FROM cached_path_reference r \
     WHERE r.reference_hash = $1 \
       AND EXISTS (SELECT 1 FROM derivation_output o WHERE o.hash = r.referrer)",
        params = [CachedPathHash];
}

/// Referrers of `missing_hash` that are **rebuildable outputs**: a
/// `derivation_output` exists for the referrer's own hash, so demoting it resets a
/// producer that rebuilds and re-pushes `missing_hash`. Producerless referrers - a
/// `.drv` (whose store-path hash is a derivation hash with no `derivation_output`)
/// or an input source - are excluded on purpose: demoting one deletes a
/// `.drv`/source the cache cannot re-supply without a full re-eval, rebuilds
/// nothing, and strands the deleted `.drv`'s own live dependents behind the
/// `.drv`-importable promotion gate - the exact dead zone this filter prevents.
async fn output_referrers_of_hash<C: ConnectionTrait>(
    db: &C,
    hash: &str,
) -> Result<Vec<String>, sea_orm::DbErr> {
    use sea_orm::FromQueryResult;

    #[derive(sea_orm::FromQueryResult)]
    struct Referrer {
        referrer: String,
    }

    Ok(
        Referrer::find_by_statement(OUTPUT_REFERRERS_SELECT.bind([hash.into()]))
            .all(db)
            .await?
            .into_iter()
            .map(|r| r.referrer)
            .collect(),
    )
}

/// A `cached_path` row whose object the uploader still owes to storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnconfirmedPath {
    pub hash: String,
    pub file_hash: String,
    pub file_size: u64,
    pub created_at: chrono::NaiveDateTime,
}

fn unconfirmed_select(limit: u64) -> Select<ECachedPath> {
    ECachedPath::find()
        .filter(CCachedPath::Confirmed.eq(false))
        .filter(CCachedPath::FileHash.is_not_null())
        .order_by_asc(CCachedPath::CreatedAt)
        .limit(limit)
}

/// The oldest `limit` unconfirmed rows: the uploader's work list.
pub async fn unconfirmed_cached_paths<C: ConnectionTrait>(
    db: &C,
    limit: u64,
) -> Result<Vec<UnconfirmedPath>, sea_orm::DbErr> {
    let rows = unconfirmed_select(limit).all(db).await?;

    Ok(rows
        .into_iter()
        .filter_map(|row| {
            Some(UnconfirmedPath {
                hash: row.hash,
                file_hash: row.file_hash?,
                file_size: row.file_size.unwrap_or(0).max(0) as u64,
                created_at: row.created_at,
            })
        })
        .collect())
}

pub async fn unconfirmed_cached_path_count<C: ConnectionTrait>(
    db: &C,
) -> Result<u64, sea_orm::DbErr> {
    ECachedPath::find()
        .filter(CCachedPath::Confirmed.eq(false))
        .count(db)
        .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn present_nar(hash: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nars").join(&hash[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{}.nar.zst", &hash[2..]));
        std::fs::write(&file, b"x").unwrap();
        (tmp, file)
    }

    /// A producerless input (`.drv` / source) whose NAR is still present must be
    /// PRESERVED: nothing rebuilds it, so demote early-returns before touching the
    /// `cached_path` row or the object. Appending no exec result is the assertion:
    /// reaching `delete_many` would fail on the missing mock result.
    #[tokio::test]
    async fn demote_preserves_a_present_producerless_object() {
        use sea_orm::{DatabaseBackend, MockDatabase};

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, file) = present_nar(hash);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()])
            .into_connection();
        let (ctx, _pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let producers = demote_cached_output(&ctx, hash).await.unwrap();

        assert!(producers.is_empty(), "a producerless input has no producer");
        assert!(
            file.exists(),
            "a present producerless object must be preserved"
        );
    }

    /// An output (a producer restores it on rebuild) whose NAR is present must have
    /// the object AND `cached_path` row removed, so a re-eval re-pushes it instead
    /// of trusting a row whose object is gone.
    #[tokio::test]
    async fn demote_deletes_a_present_output_object() {
        use gradient_types::ids::{DerivationId, DerivationOutputId};
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
        use std::collections::BTreeMap;

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, file) = present_nar(hash);

        let output = gradient_entity::derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation: DerivationId::now_v7(),
            hash: hash.to_string(),
            ..Default::default()
        };

        // Find the output, RETURNING the demoted row, take both lock classes in
        // order, drop its producer's upstream trust, retire the `cached_path` row
        // (no reverse ripple: it was not whole), clear `is_cached` and run the
        // readiness pass; then the object is removed.
        let retired = BTreeMap::from([
            ("hash".to_owned(), Value::from(hash.to_owned())),
            ("was_whole".to_owned(), Value::from(false)),
        ]);
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![output.clone()], vec![output]])
            .append_query_results([vec![retired]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                7
            ])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let producers = demote_cached_output(&ctx, hash).await.unwrap();

        assert_eq!(producers.len(), 1, "the output's producer is returned");
        assert!(!file.exists(), "demote must delete the output's NAR object");
        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert!(
            log.iter()
                .any(|s| s.contains("DELETE FROM cached_path") && s.contains("was_whole")),
            "the row must be retired, so the counters it backed move with it: {log:?}"
        );
        assert_eq!(
            log.len(),
            15,
            "outputs, demote, path lock, anchor lock, trust clear, retire lock, delete, \
             is_cached, producers of the union, producers of what is gone, owners, \
             un-promote, and the raised, locked recompute of what the producers now \
             demand: {log:?}"
        );
        let paths = log
            .iter()
            .position(|s| s.contains("FROM cached_path WHERE hash = ANY($1)"))
            .expect("the path lock is taken first");
        let anchors = log
            .iter()
            .position(|s| s.contains("FROM derivation_build WHERE derivation = ANY($1::uuid[])"))
            .expect("the anchor lock is taken");
        let trust = log
            .iter()
            .position(|s| s.contains("SET substitutable = false"))
            .expect("the upstream trust is dropped");
        let retire = log
            .iter()
            .position(|s| s.contains("DELETE FROM cached_path"))
            .expect("the row is retired");
        assert!(
            paths < trust,
            "this is the one path that writes derivation_build before a retire, so it takes the cached_path lock first or it deadlocks against a concurrent eviction: {log:?}"
        );
        assert!(
            paths < anchors && anchors < trust,
            "the trust clear names its producers in one UPDATE, so it acquires them in \
             plan order: the ordered anchor pass has to precede it or it deadlocks \
             against the readiness repair's chunk: {log:?}"
        );
        assert!(
            trust < retire,
            "the retire decides fetchability, so the stale offer must be gone first: {log:?}"
        );
        let terminal_success = crate::status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS);
        assert!(
            !log.iter()
                .any(|s| s.contains(&format!("status IN ({terminal_success})"))),
            "the producer reset belongs to the retire, which decides it from the flag it just wrote: {log:?}"
        );
    }

    /// A hash with NO `cached_path` row is half of what
    /// [`unbacked_trusted_outputs_select`] matches, and it deletes nothing and
    /// ripples nothing, so a readiness pass keyed only on what moved would leave its
    /// producer terminal-success and fetchable against an artifact that does not
    /// exist. `REQUEUEABLE` excludes terminal success, so no other path recovers it.
    #[tokio::test]
    async fn demote_of_a_hash_with_no_row_still_resets_its_producer() {
        use gradient_types::ids::{DerivationId, DerivationOutputId};
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
        use std::collections::BTreeMap;

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, _file) = present_nar(hash);
        let producer = DerivationId::now_v7();
        let output = gradient_entity::derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation: producer,
            hash: hash.to_string(),
            ..Default::default()
        };
        let drv_row =
            BTreeMap::from([("derivation".to_owned(), Value::from(producer.into_inner()))]);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![output.clone()], vec![output]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![drv_row.clone()]])
            .append_query_results([vec![drv_row.clone()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![drv_row]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results(vec![
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                };
                7
            ])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        demote_cached_output(&ctx, hash).await.unwrap();

        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(
            log.len(),
            18,
            "outputs, demote, path lock, anchor lock, trust clear, retire lock, delete, \
             producers of the union, anchor lock, mark, ripple, producers of what is \
             gone, reset, owners, un-promote, and the raised, locked recompute of what \
             the producers now demand: {log:?}"
        );
        assert!(
            !log.iter()
                .any(|s| s.contains("WHERE is_cached AND hash = ANY($1)")),
            "nothing was deleted, so the retire's own clears never run: {log:?}"
        );
        assert!(
            log.iter()
                .any(|s| s.contains("FROM derivation_output o WHERE o.hash = ANY($1)")),
            "the producers of the asked-for hash are still resolved: {log:?}"
        );
        assert!(
            log.iter().any(|s| s.contains("SET fetchable = false")),
            "and offered to the mark, which decides from the predicate: {log:?}"
        );
        assert!(
            log.iter().any(|s| s.contains("AND NOT db.fetchable")),
            "so the producer with nothing left to serve is reset: {log:?}"
        );
    }

    /// Demote must clear `external_url` too, not just `is_cached` - otherwise the
    /// node still records an upstream offer for a copy that is gone, and its
    /// reset-to-build anchor dead-ends on a `.drv` the eval never re-pushes.
    #[test]
    fn demote_clears_upstream_availability() {
        use sea_orm::ActiveValue::Set;

        let o = gradient_entity::derivation_output::Model {
            is_cached: true,
            cached_path: Some(gradient_types::ids::CachedPathId::now_v7()),
            external_url: Some("https://cache.example/x.narinfo".to_string()),
            nar_hash: Some("sha256:aaa".to_string()),
            file_hash: Some("sha256:bbb".to_string()),
            file_size: Some(42),
            ..Default::default()
        };

        let am = demoted_output(o);
        assert_eq!(am.is_cached, Set(false));
        assert_eq!(am.cached_path, Set(None));
        assert_eq!(
            am.external_url,
            Set(None),
            "external_url must be cleared so no upstream offer survives the demote"
        );
        assert_eq!(am.nar_hash, Set(None));
        assert_eq!(am.file_hash, Set(None));
        assert_eq!(am.file_size, Set(None));
    }

    #[test]
    fn file_size_sum_casts_to_bigint() {
        use gradient_entity::cached_path::Entity as ECP;
        use sea_orm::{DatabaseBackend, EntityTrait, QuerySelect, QueryTrait};
        let sql = ECP::find()
            .select_only()
            .column_as(file_size_sum_bigint(), "total")
            .build(DatabaseBackend::Postgres)
            .to_string();
        assert!(sql.to_uppercase().contains("CAST"), "missing cast: {sql}");
        assert!(
            sql.to_lowercase().contains("bigint"),
            "missing bigint: {sql}"
        );
    }

    /// The reconciler enforces the row-vs-object invariant on terminal-success
    /// anchors: any output with no backing NAR is demoted. It must key on the
    /// **ground truth** (a missing `cached_path` NAR), NOT the derived
    /// `is_cached` flag - that flag is `false` for the never-cached-output
    /// dead zone this sweep must rescue - and must skip upstream-fetchable outputs
    /// (`external_url`).
    #[test]
    fn unbacked_trusted_select_matches_the_gate() {
        let sql = unbacked_trusted_outputs_select()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let terminal_success = crate::status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS);
        assert!(
            sql.contains(&format!("db.status IN ({terminal_success})")),
            "must mirror the gate's success states: {sql}"
        );
        assert!(
            !sql.contains("o.is_cached"),
            "must NOT gate on is_cached (it is false for the never-cached-output dead zone): {sql}"
        );
        assert!(
            sql.contains("o.external_url IS NULL"),
            "must skip upstream-served outputs: {sql}"
        );
        assert!(
            sql.contains("NOT EXISTS") && sql.contains("cp.file_hash IS NOT NULL"),
            "must require a missing backing NAR: {sql}"
        );
    }

    /// The heal grants ONE rebuild and then stops (#654): the demote arm is the
    /// invariant minus the anchors a real build has already been spent on. A relay
    /// attempt is not that evidence - it moves upstream bytes and never proves what
    /// a build lands - so the term reads real builds only.
    #[test]
    fn the_demote_arm_skips_an_anchor_the_fleet_has_already_built() {
        let norm = |sql: String| sql.split_whitespace().collect::<Vec<_>>().join(" ");
        let sql = norm(demotable_unbacked_outputs_select());
        let success = crate::status_sql::attempt_outcome_in(&AttemptOutcome::SUCCESS);
        assert!(
            sql.starts_with(&norm(unbacked_trusted_outputs_select())),
            "the demote arm must be the invariant itself, narrowed: {sql}"
        );
        assert!(
            sql.contains(
                "AND NOT EXISTS (SELECT 1 FROM build_attempt ba WHERE ba.derivation_build = db.id"
            ),
            "narrowed by the anchor's own attempts: {sql}"
        );
        assert!(
            sql.contains("NOT ba.substitute")
                && sql.contains(&format!("ba.outcome IN ({success})")),
            "a relay attempt is not proof a build was tried: {sql}"
        );
    }

    /// The other arm: a producer whose granted rebuild came and went without
    /// backing its output is failed, not demoted again. `FailedPermanent` is what
    /// stops it - the anchor was terminal-*success*, which no requeue path reaches,
    /// so its dependents counted it unready forever with nothing ever dispatched to
    /// report the output missing.
    #[test]
    fn the_verdict_arm_fails_a_producer_whose_settled_build_left_an_output_unbacked() {
        let sql = fail_unhealable_anchors_sql()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            sql.contains(&format!(
                "SET status = {}",
                crate::status_sql::build(BuildStatus::FailedPermanent)
            )),
            "the verdict is a terminal failure: {sql}"
        );
        assert!(
            sql.contains(&format!(
                "db.status IN ({})",
                crate::status_sql::build_in(&BuildStatus::TERMINAL_SUCCESS)
            )),
            "only an anchor the graph still trusts can be failed by this: {sql}"
        );
        assert!(
            sql.contains("ba.build_finished_at < $1"),
            "a build whose NAR commits may still be in flight is left alone: {sql}"
        );
        assert!(
            sql.contains("o.external_url IS NULL") && sql.contains("cp.file_hash IS NOT NULL"),
            "the output must be in neither our cache nor an upstream: {sql}"
        );
        assert!(
            sql.contains("AS missing"),
            "the failure names the output the build never backed: {sql}"
        );
    }

    /// The loop #654 reported, in one pass: the demote arm's hash reaches the
    /// demote, and the verdict arm runs once after it. Appending exactly these
    /// results is the assertion - a second demote would fail on a missing mock,
    /// which is what the old sweep did on every reconcile pass forever.
    #[tokio::test]
    async fn the_sweep_demotes_what_it_can_heal_and_then_asks_for_the_rest() {
        use sea_orm::{DatabaseBackend, MockDatabase, Value};
        use std::collections::BTreeMap;

        let fresh = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, _file) = present_nar(fresh);

        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![BTreeMap::from([(
                "hash".to_owned(),
                Value::from(fresh.to_owned()),
            )])]])
            // The hash reaches the demote, which finds no producer and, its object
            // being present, preserves it and returns before any write.
            .append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()])
            // The verdict arm finds nothing to fail.
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let sweep = demote_unbacked_trusted_outputs(&ctx).await.unwrap();

        assert_eq!(sweep, UnbackedSweep::default());
        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        assert_eq!(
            log.len(),
            3,
            "the demote arm, one demote, the verdict: {log:?}"
        );
        assert!(
            log[1].contains(fresh),
            "the demote is handed the hash: {log:?}"
        );
        assert!(
            log[2].contains("SET status ="),
            "the verdict runs even when the demote arm moved nothing: {log:?}"
        );
    }

    /// The verdict records WHY on the attempt that reported success, and that
    /// reason is deterministic, so the next evaluation does not thaw the anchor
    /// into a rebuild that reproduces it. Without the attempt the UI shows a failed
    /// build with no cause, and the thaw re-queues it once per evaluation forever.
    #[tokio::test]
    async fn the_verdict_records_the_reason_on_the_attempt_that_claimed_success() {
        use gradient_entity::build_attempt::{AttemptFailureReason, Model as MAttempt};
        use sea_orm::{DatabaseBackend, MockDatabase, MockExecResult, Value};
        use std::collections::BTreeMap;

        let tmp = tempfile::tempdir().unwrap();
        let anchor = DerivationBuildId::now_v7();
        let derivation = DerivationId::now_v7();
        let missing = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";

        let failed = BTreeMap::from([
            (
                "derivation".to_owned(),
                Value::from(derivation.into_inner()),
            ),
            ("anchor".to_owned(), Value::from(anchor.into_inner())),
            ("from_status".to_owned(), Value::from(3i32)),
            ("to_status".to_owned(), Value::from(4i32)),
            ("missing".to_owned(), Value::from(missing.to_owned())),
        ]);
        let attempt = MAttempt {
            derivation_build: anchor,
            outcome: AttemptOutcome::Built,
            ..Default::default()
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            // nothing left to demote, one anchor to fail, its latest attempt
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![failed]])
            .append_query_results([vec![attempt.clone()], vec![attempt]])
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let sweep = demote_unbacked_trusted_outputs(&ctx).await.unwrap();

        assert_eq!(sweep.failed, 1, "the anchor is failed");
        drop(ctx);
        let log = crate::pool::statements(pool.into_transaction_log());
        // The log renders each statement with `Debug`, so its quoted identifiers
        // are escaped; the values are what this asserts on anyway.
        let rewrite = log
            .iter()
            .find(|s| s.contains("SET") && s.contains("outcome") && s.contains(missing))
            .unwrap_or_else(|| panic!("the attempt must be rewritten: {log:?}"));
        assert!(
            rewrite.contains(&format!(
                "Int(Some({}))",
                crate::status_sql::attempt_reason(AttemptFailureReason::OutputMissing)
            )),
            "with the deterministic reason the thaw reads: {rewrite}"
        );
        assert!(
            rewrite.contains(&format!(
                "Int(Some({}))",
                crate::status_sql::attempt_outcome(AttemptOutcome::Failed)
            )),
            "and the outcome that pairs with it: {rewrite}"
        );
    }

    /// A producerless input (source / `.drv`) whose NAR is still present must be
    /// PRESERVED by `demote_cached_output`: nothing rebuilds it, so deleting the
    /// only copy dead-ends every dependent on `InputsUnavailable`. An output (has
    /// a producer, restored by the rebuild) or a genuinely-absent object is never
    /// preserved.
    #[test]
    fn preserve_only_a_present_producerless_artifact() {
        assert!(
            preserve_missing_artifact(false, true),
            "producerless + present must be kept (transient fetch miss, not a zombie)"
        );
        assert!(
            !preserve_missing_artifact(false, false),
            "producerless + gone is a zombie to purge"
        );
        assert!(
            !preserve_missing_artifact(true, true),
            "an output is demoted so its producer rebuilds it"
        );
        assert!(!preserve_missing_artifact(true, false));
    }

    /// `demote_referrers_of` may only demote referrers that are rebuildable
    /// outputs. A producerless `.drv`/source referrer must be excluded: deleting it
    /// re-pushes nothing (no producer rebuilds) and strands the `.drv`'s own live
    /// dependents behind the `.drv`-importable promotion gate - a permanent dead
    /// zone (a completed, substitutable dep whose `.drv` a demote deleted, blocking
    /// every non-substitutable dependent from ever dispatching).
    #[test]
    fn output_referrers_exclude_producerless_drv_and_source() {
        let sql = OUTPUT_REFERRERS_SELECT
            .text()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(
            sql.contains("FROM cached_path_reference r") && sql.contains("r.reference_hash = $1"),
            "must resolve referrers of the missing hash: {sql}"
        );
        assert!(
            sql.contains("EXISTS (SELECT 1 FROM derivation_output o WHERE o.hash = r.referrer)"),
            "must require the referrer to be a producing output, excluding .drv/source: {sql}"
        );
    }

    #[test]
    fn zero_limit_is_unlimited() {
        assert_eq!(limit_to_bytes(0), None);
        assert_eq!(limit_to_bytes(-5), None);
        assert_eq!(limit_to_bytes(1), Some(BYTES_PER_GB));
    }

    #[test]
    fn headroom_bounded_by_tighter_axis() {
        let five_mb = 5 * 1024 * 1024;
        let used = BYTES_PER_GB - five_mb;
        assert_eq!(headroom(1, used, 0, 0), five_mb);
    }

    #[test]
    fn headroom_instance_axis_can_dominate() {
        let one_mb = 1024 * 1024;
        let inst_used = BYTES_PER_GB - one_mb;
        assert_eq!(headroom(0, 0, 1, inst_used), one_mb);
    }

    #[test]
    fn both_unlimited_is_max() {
        assert_eq!(headroom(0, 9_999, 0, 9_999), i64::MAX);
    }

    #[test]
    fn the_uploader_scan_reads_the_partial_index_oldest_first() {
        use sea_orm::{DatabaseBackend, QueryTrait};

        let sql = unconfirmed_select(1000)
            .build(DatabaseBackend::Postgres)
            .to_string()
            .to_uppercase();
        assert!(
            sql.contains(r#""CACHED_PATH"."CONFIRMED" = FALSE"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#""CACHED_PATH"."FILE_HASH" IS NOT NULL"#),
            "{sql}"
        );
        assert!(
            sql.contains(r#"ORDER BY "CACHED_PATH"."CREATED_AT" ASC"#),
            "{sql}"
        );
        assert!(sql.ends_with("LIMIT 1000"), "{sql}");
    }
}
