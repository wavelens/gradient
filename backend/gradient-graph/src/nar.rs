/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! Recording a stored NAR in the cache index.

use anyhow::Context as _;
use gradient_db::{DbContext, WorkerDb};
use gradient_entity::StorePath;
use gradient_types::ids::{CacheId, CachedPathId, CachedPathSignatureId};
use gradient_types::*;
use gradient_util::nix_hash::{is_nix32_hash, normalize_nar_hash};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, IntoActiveModel, QueryFilter,
    QuerySelect, Set,
};
use tracing::{debug, trace, warn};

use crate::messages::{NarCommit, NarCommitted, NarConfirm, SignTargets};

/// Record a stored NAR: the row, its reference index, the counter seeded from it,
/// the wholeness flip that counter makes, and the anchor side of that flip.
///
/// Runs inside the graph actor's transaction, and only there. Both the reference
/// endpoints and the pre-commit wholeness endpoint are read under locks that a
/// pooled handle would release with the statement that took them, which is a race
/// against the three maintenance retires with no compile error and no runtime
/// signal (see `gradient_db::nar_closure`), so the handle is checked here instead.
/// The anchor locks are taken on that same transaction, always after the
/// `cached_path` ones and never before.
pub(crate) async fn commit(ctx: &DbContext, c: &NarCommit) -> anyhow::Result<NarCommitted> {
    let db = &ctx.worker_db;
    let sp = StorePath::parse(&c.store_path).map_err(|e| anyhow::anyhow!("{e}"))?;
    if !is_nix32_hash(sp.hash()) {
        anyhow::bail!("malformed store path: {}", c.store_path);
    }

    let txn = db.as_transaction().context(
        "NarCommit must run inside a transaction: the pre-commit wholeness endpoint and the reference locks are only held under one",
    )?;
    let lock = gradient_db::lock_reference_endpoints(txn, sp.hash(), &c.references).await?;

    let Upserted {
        cached_path,
        created,
        was_whole,
        was_backed,
    } = upsert_cached_path(db, sp.hash(), sp.name(), c).await?;

    sync_reference_index(db, sp.hash(), &c.references).await?;

    let producers = gradient_db::producers_of_hashes(txn, &[sp.hash().to_owned()]).await?;

    // The NAR is where a built output's runtime references are learned, so the
    // graph edges they name are written from the same report the index is, and what
    // demand those edges carry is recomputed from the anchor they hang off.
    let referenced = gradient_db::producers_of_tokens(txn, &c.references).await?;
    if !referenced.is_empty() {
        for producer in &producers {
            gradient_db::insert_runtime_edges(txn, *producer, &referenced).await?;
        }

        let moved = gradient_db::recompute_demand(txn, &producers).await?;
        let mut changes = gradient_db::promote(txn, &moved.gained).await?;
        changes.extend(gradient_db::unpromote_ungated(txn, &moved.lost).await?);
        gradient_db::emit_transition_effects(ctx, &changes).await;
    }

    // Wholeness is counted on the anchor, and the seed needs the endpoint this
    // commit destroyed: a path that had no NAR before is what makes its producers
    // present, and the row already says so by the time the seed reads it. A re-push
    // of a backed path changed no presence, so it is only recounted.
    let (freshly_present, recounted): (&[DerivationId], &[DerivationId]) = if was_backed {
        (&[], &producers)
    } else {
        (&producers, &[])
    };
    let seeded = gradient_db::seed_runtime_deps(txn, freshly_present, recounted).await?;
    let owners = if was_backed {
        Vec::new()
    } else {
        gradient_db::derivations_with_hashes(txn, &[sp.hash().to_owned()]).await?
    };
    advance_anchors(ctx, txn, &seeded.whole, &owners).await?;
    if !seeded.unwhole.is_empty() {
        warn!(store_path = %c.store_path, unwhole = seeded.unwhole.len(), "commit added unwhole references");
        retract_anchors(ctx, txn, &seeded.unwhole).await?;
    }

    // The path counter is the reader that has not moved yet; it converges on its
    // own and nothing reads it for a gate any more.
    let whole = gradient_db::seed_references(&lock).await?;
    match (was_whole, whole) {
        (false, true) => {
            let moved = gradient_db::ripple_whole(db, vec![sp.hash().to_owned()]).await?;
            trace!(store_path = %c.store_path, whole = moved.len(), "reference closure ripple");
        }
        (true, false) => {
            gradient_db::ripple_unwhole(db, vec![sp.hash().to_owned()]).await?;
        }
        _ => {}
    }

    queue_signature_placeholders(db, cached_path, c.targets).await?;
    let outputs_marked = EDerivationOutput::update_many()
        .col_expr(CDerivationOutput::IsCached, Expr::value(true))
        .col_expr(CDerivationOutput::CachedPath, Expr::value(cached_path))
        .filter(CDerivationOutput::Hash.eq(sp.hash()))
        .exec(db)
        .await
        .context("mark derivation outputs cached")?
        .rows_affected;
    trace!(store_path = %c.store_path, outputs_marked, created, "cached path committed");

    Ok(NarCommitted {
        cached_path,
        created,
        outputs_marked,
    })
}

/// The anchor side of a forward wholeness flip: the anchors in `whole` can serve
/// their outputs, and the `owners` of a `.drv` this commit made present are
/// importable, so their gates may have opened.
///
/// One ordered lock over both sets, on the commit's own transaction: the mark is a
/// bound and not a claim, so an anchor whose predicate does not hold is passed over,
/// and `promote` runs under the same lock that produced its candidates.
async fn advance_anchors(
    ctx: &DbContext,
    txn: &sea_orm::DatabaseTransaction,
    whole: &[DerivationId],
    owners: &[DerivationId],
) -> anyhow::Result<()> {
    let lock = gradient_db::lock_anchors(txn, &union(whole, owners)).await?;
    let mut changes = gradient_db::became_fetchable(&lock).await?;
    changes.extend(gradient_db::promote(txn, owners).await?);
    gradient_db::emit_transition_effects(ctx, &changes).await;

    Ok(())
}

/// The symmetric loss. A commit that reports a reference we do not have takes
/// anchors OUT of wholeness, and one left fetchable against such an anchor
/// dispatches a build whose input nothing can provide - the forward-only half of
/// this pair is the dead zone this project has paid for repeatedly.
async fn retract_anchors(
    ctx: &DbContext,
    txn: &sea_orm::DatabaseTransaction,
    unwhole: &[DerivationId],
) -> anyhow::Result<()> {
    let lock = gradient_db::lock_anchors(txn, unwhole).await?;
    let changes = gradient_db::lost_fetchability(&lock).await?;
    gradient_db::emit_transition_effects(ctx, &changes).await;

    Ok(())
}

fn union(a: &[DerivationId], b: &[DerivationId]) -> Vec<DerivationId> {
    let mut all = a.to_vec();
    all.extend_from_slice(b);
    all.sort_unstable();
    all.dedup();

    all
}

/// The debug-info walk decompresses the whole NAR, so it runs detached.
pub(crate) fn after_commit(ctx: &DbContext, committed: &NarCommitted, store_path: &str) {
    let Ok(sp) = StorePath::parse(store_path) else {
        return;
    };
    if !gradient_db::carries_debug_info(sp.name()) {
        return;
    }

    let db = ctx.worker_db.detached();
    let nar_storage = ctx.storage.nar_storage.clone();
    let hash = sp.hash().to_owned();
    let cached_path = committed.cached_path;
    ctx.shutdown.spawn(async move {
        match gradient_db::index_cached_path(&db, &nar_storage, cached_path, &hash).await {
            Ok(0) => {}
            Ok(count) => debug!(%hash, count, "indexed debug-info build ids"),
            Err(e) => warn!(%hash, error = %e, "failed to index debug info"),
        }
    });
}

/// The row the commit wrote, and what it was before: `was_whole` is the
/// pre-commit endpoint of the wholeness flip, which no statement after this write
/// can recover (see `gradient_db::seed_references`). It is read under the row
/// lock, so a maintenance retire rippling the same counter from its own
/// transaction cannot land between the read and the write.
struct Upserted {
    cached_path: CachedPathId,
    created: bool,
    was_whole: bool,
    /// The row had a NAR before this commit. False is what makes the path's
    /// producers freshly present, which no statement after the write can recover.
    was_backed: bool,
}

/// Insert or refresh the row under its `FOR UPDATE` lock. A duplicate-key error
/// on the insert propagates: the actor serialises commits and
/// `lock_reference_endpoints` holds this row `FOR SHARE` before this runs, so
/// there is no race to recover from, and inside a transaction a re-select after
/// a failed INSERT would only replace the real error with 25P02.
async fn upsert_cached_path(
    db: &WorkerDb,
    hash: &str,
    package: &str,
    c: &NarCommit,
) -> anyhow::Result<Upserted> {
    match ECachedPath::find()
        .filter(CCachedPath::Hash.eq(hash))
        .lock_exclusive()
        .one(db)
        .await?
    {
        Some(row) => {
            let id = row.id;
            let was_whole = row.is_whole();
            let was_backed = row.is_fully_cached();
            let file_hash = normalize_nar_hash(&c.file_hash);
            // Different bytes under the same store path: the recorded build-id
            // members no longer describe the NAR, so re-open it to the indexer.
            let rescan_debug_info = row.file_hash.as_deref() != Some(file_hash.as_str());
            let was_confirmed = row.confirmed;
            let mut active = row.into_active_model();
            active.file_size = Set(Some(c.file_size));
            active.file_hash = Set(Some(file_hash));
            if rescan_debug_info {
                active.debug_info_indexed = Set(false);
                active.confirmed = Set(c.confirmed);
            } else if c.confirmed && !was_confirmed {
                active.confirmed = Set(true);
            }

            active.nar_size = Set(Some(c.nar_size));
            active.nar_hash = Set(Some(normalize_nar_hash(&c.nar_hash)));
            active.references = Set(Some(c.references.join(" ")));
            if c.deriver.is_some() {
                active.deriver = Set(c.deriver.clone());
            }

            if c.ca.is_some() {
                active.ca = Set(c.ca.clone());
            }

            active.update(db).await?;
            Ok(Upserted {
                cached_path: id,
                created: false,
                was_whole,
                was_backed,
            })
        }
        None => {
            let am = MCachedPath {
                id: CachedPathId::now_v7(),
                hash: hash.to_owned(),
                package: package.to_owned(),
                file_hash: Some(normalize_nar_hash(&c.file_hash)),
                file_size: Some(c.file_size),
                nar_size: Some(c.nar_size),
                nar_hash: Some(normalize_nar_hash(&c.nar_hash)),
                deriver: c.deriver.clone(),
                ca: c.ca.clone(),
                references: Some(c.references.join(" ")),
                created_at: now(),
                confirmed: c.confirmed,
                ..Default::default()
            }
            .into_active_model();

            let row = am.insert(db).await?;
            Ok(Upserted {
                cached_path: row.id,
                created: true,
                was_whole: false,
                was_backed: false,
            })
        }
    }
}

/// Mark a relayed path's object as stored. Answers `false` when the row's bytes
/// moved on since the upload began.
pub(crate) async fn confirm(ctx: &DbContext, c: &NarConfirm) -> anyhow::Result<bool> {
    let updated = ECachedPath::update_many()
        .col_expr(CCachedPath::Confirmed, Expr::value(true))
        .filter(CCachedPath::Hash.eq(c.hash.as_str()))
        .filter(CCachedPath::FileHash.eq(normalize_nar_hash(&c.file_hash)))
        .filter(CCachedPath::Confirmed.eq(false))
        .exec(&ctx.worker_db)
        .await
        .context("confirm cached path")?
        .rows_affected;

    Ok(updated == 1)
}

gradient_db::sql! {
    SYNC_REFERENCE_INDEX = r#"
        WITH reported(reference, ord) AS (
            SELECT DISTINCT ON (t.tok) t.tok, t.ord
            FROM unnest($2::text[]) WITH ORDINALITY AS t(tok, ord)
            WHERE t.tok <> ''
            ORDER BY t.tok, t.ord
        ), dropped AS (
            DELETE FROM cached_path_reference r
            WHERE r.referrer = $1
              AND NOT EXISTS (SELECT 1 FROM reported n WHERE n.reference = r.reference)
        )
        INSERT INTO cached_path_reference (referrer, reference, reference_hash, position)
        SELECT $1, n.reference, split_part(n.reference, '-', 1), n.ord FROM reported n
        ON CONFLICT (referrer, reference) DO UPDATE
        SET position = EXCLUDED.position
        WHERE cached_path_reference.position IS DISTINCT FROM EXCLUDED.position
        "#,
        params = [CachedPathHash, CachedPathHashes(64)];
}

/// Record a path's hash-name references in the normalized `cached_path_reference`
/// relation: `reference_hash` indexes referrer lookups, and `position` preserves
/// the worker's order (nix store-path order) so the narinfo `References:` line and
/// signature fingerprint reconstruct verbatim.
///
/// Authoritative, not add-only: afterwards the referrer's rows are exactly
/// `references`, and an empty report is a report that clears them. The same store
/// path CAN come back with a different reference set - an input-addressed path
/// rebuilt non-deterministically keeps its hash while its closure moves - and
/// `cached_path.missing_references` is counted from this table by both
/// [`gradient_db::seed_references`] and `gradient_db::repair_counters_for`, so an
/// edge an add-only write left behind over-counts that path forever: the repair
/// recomputes from the same stale row and can never disagree with it.
///
/// One statement, so the prune and the upsert read one snapshot and touch
/// disjoint rows. `position` is rewritten only where it moved, so an unchanged
/// re-ingest writes no row version; dropping one reference shifts every later
/// one, and a stale position reorders both the `References:` line and the
/// fingerprint that is signed over it. Duplicate tokens fold first, since
/// `DO UPDATE` refuses to touch one row twice.
async fn sync_reference_index(
    db: &WorkerDb,
    hash: &str,
    references: &[String],
) -> Result<(), sea_orm::DbErr> {
    db.execute_raw(SYNC_REFERENCE_INDEX.bind([hash.into(), references.to_vec().into()]))
        .await?;

    Ok(())
}

async fn queue_signature_placeholders(
    db: &WorkerDb,
    cached_path: CachedPathId,
    targets: SignTargets,
) -> anyhow::Result<()> {
    let cache_ids: Vec<CacheId> = match targets {
        SignTargets::None => vec![],
        SignTargets::Cache(id) => vec![id],
        SignTargets::ProjectCaches(project) => EProjectCache::find()
            .filter(CProjectCache::Project.eq(project))
            .all(db)
            .await?
            .into_iter()
            .map(|oc| oc.cache)
            .collect(),
    };

    if cache_ids.is_empty() {
        return Ok(());
    }

    let ts = now();
    let rows: Vec<ACachedPathSignature> = cache_ids
        .into_iter()
        .map(|cid| {
            MCachedPathSignature {
                id: CachedPathSignatureId::now_v7(),
                cached_path,
                cache: cid,
                created_at: ts,
                ..Default::default()
            }
            .into_active_model()
        })
        .collect();

    let result = ECachedPathSignature::insert_many(rows)
        .on_conflict(
            OnConflict::columns([
                CCachedPathSignature::CachedPath,
                CCachedPathSignature::Cache,
            ])
            .do_nothing()
            .to_owned(),
        )
        .try_insert()
        .exec(db)
        .await;
    if let Err(e) = result {
        warn!(%cached_path, error = %e, "insert cached_path_signature failed");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_ctx::ctx;
    use gradient_types::ids::ProjectId;
    use sea_orm::{
        DatabaseBackend, DatabaseConnection, MockDatabase, MockExecResult, Statement,
        TransactionTrait, Value,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use uuid::Uuid;

    const SP: &str = "/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-hello-2.12";
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    /// A referrer of [`HASH`], for the ripple level a whole commit drives.
    const DEP_HASH: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn cache_id() -> CacheId {
        CacheId::new(Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap())
    }

    fn project() -> ProjectId {
        ProjectId::new(Uuid::parse_str("20000000-0000-0000-0000-000000000003").unwrap())
    }

    fn project_cache_row() -> gradient_entity::project_cache::Model {
        gradient_entity::project_cache::Model {
            project: project(),
            cache: cache_id(),
            ..Default::default()
        }
    }

    fn returned_cached_path(hash: &str) -> MCachedPath {
        MCachedPath {
            id: CachedPathId::new(Uuid::now_v7()),
            hash: hash.to_string(),
            package: "hello-2.12".to_string(),
            file_hash: Some("sha256:abc".to_string()),
            file_size: Some(5),
            nar_size: Some(5),
            nar_hash: Some("sha256:def".to_string()),
            created_at: now(),
            ..Default::default()
        }
    }

    fn commit_for(store_path: &str) -> NarCommit {
        NarCommit {
            store_path: store_path.to_owned(),
            file_hash: "sha256:abc".to_owned(),
            file_size: 5,
            nar_size: 5,
            nar_hash: "sha256:def".to_owned(),
            references: Vec::new(),
            deriver: None,
            ca: None,
            targets: SignTargets::None,
            confirmed: true,
        }
    }

    /// One row of a producer lookup, which projects the derivation alone.
    fn producer_row() -> BTreeMap<String, Value> {
        BTreeMap::from([("derivation".to_owned(), Value::from(Uuid::now_v7()))])
    }

    /// One region row of the bounded demand walk: the anchor and what it now reads.
    fn demand_row() -> BTreeMap<String, Value> {
        BTreeMap::from([
            ("derivation".to_owned(), Value::from(Uuid::now_v7())),
            ("demanded".to_owned(), Value::from(true)),
        ])
    }

    /// The seed's reply: whether the path is whole after the recount.
    fn seed_reply(whole: bool) -> Vec<BTreeMap<String, Value>> {
        vec![BTreeMap::from([("whole".to_owned(), Value::from(whole))])]
    }

    /// A ripple level reads its referrers before it moves them, so a level that
    /// runs takes two query results rather than one.
    fn referrer_counts(referrer: &str, n: i32) -> Vec<BTreeMap<String, Value>> {
        vec![BTreeMap::from([
            ("referrer".to_owned(), Value::from(referrer.to_owned())),
            ("n".to_owned(), Value::from(n)),
        ])]
    }

    fn exec(rows_affected: u64) -> MockExecResult {
        MockExecResult {
            last_insert_id: 0,
            rows_affected,
        }
    }

    fn statements(db: WorkerDb) -> Vec<String> {
        gradient_db::pool::statements(db.into_transaction_log())
    }

    /// The statements as sea-orm built them. The shared helper formats each with
    /// `{:?}`, which escapes the quotes sea-orm puts around every identifier, so
    /// an assertion on a generated statement reads the raw `sql` instead.
    fn raw_statements(db: WorkerDb) -> Vec<Statement> {
        gradient_db::pool::raw_statements(db.into_transaction_log())
    }

    /// The value a generated statement binds to `column`, through the placeholder
    /// the SET clause or the insert's column list gives it. Searching the value
    /// list for the bare `Bool(Some(false))` would match the `debug_info_indexed`
    /// these same statements write, and pass while `confirmed` went the other way.
    fn bound(stmt: &Statement, column: &str) -> Value {
        let quoted = format!("\"{column}\"");
        let position = match stmt.sql.split_once(&format!("{quoted} = $")) {
            Some((_, rest)) => rest
                .chars()
                .take_while(char::is_ascii_digit)
                .collect::<String>()
                .parse::<usize>()
                .expect("a placeholder number"),
            None => {
                let columns = stmt
                    .sql
                    .split_once('(')
                    .and_then(|(_, rest)| rest.split_once(')'))
                    .expect("an insert column list")
                    .0;
                columns
                    .split(", ")
                    .position(|c| c == quoted)
                    .unwrap_or_else(|| panic!("{column} is not written: {}", stmt.sql))
                    + 1
            }
        };

        stmt.values.as_ref().expect("the statement binds values").0[position - 1].clone()
    }

    /// `commit` runs in the graph actor's transaction and rejects a pooled handle,
    /// so every test drives one. The mock records the whole transaction as a single
    /// log entry whose synthetic `BEGIN`/`COMMIT` the shared helper drops, so the
    /// statement indices stay the ones the code issues.
    async fn commit_in_transaction(ctx: &DbContext, c: &NarCommit) -> anyhow::Result<NarCommitted> {
        let tx = Arc::new(ctx.worker_db.begin().await.expect("begin"));
        let scoped = ctx.in_transaction(Arc::clone(&tx));
        let committed = commit(&scoped, c).await;
        drop(scoped);
        Arc::try_unwrap(tx)
            .expect("no handle outlives the commit")
            .commit()
            .await
            .expect("commit the transaction");

        committed
    }

    /// The scripted commit, its context dropped so the pool handle the log is read
    /// from is the last one alive.
    async fn commit_and_log(db: DatabaseConnection, c: &NarCommit) -> Vec<String> {
        let (ctx, pool) = ctx(db).await;
        commit_in_transaction(&ctx, c).await.expect("commit");
        drop(ctx);

        statements(pool)
    }

    /// [`commit_and_log`] over the raw statements.
    async fn commit_and_raw_log(db: DatabaseConnection, c: &NarCommit) -> Vec<Statement> {
        let (ctx, pool) = ctx(db).await;
        commit_in_transaction(&ctx, c).await.expect("commit");
        drop(ctx);

        raw_statements(pool)
    }

    /// An existing whole row re-pushed with `references`, seeded back to whole so
    /// no ripple runs: the reference index is the only thing under test. A report
    /// that names references draws the runtime-edge producer lookup as well, which
    /// answers with none so no edge is written.
    async fn recommit_log(references: Vec<String>) -> Vec<String> {
        let mut mock = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![returned_cached_path(HASH)],
                vec![returned_cached_path(HASH)],
            ])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()]);
        if !references.is_empty() {
            mock = mock.append_query_results([Vec::<BTreeMap<String, Value>>::new()]);
        }

        let db = mock
            .append_query_results([seed_reply(true)])
            .append_exec_results([exec(0), exec(1), exec(1)])
            .into_connection();

        commit_and_log(
            db,
            &NarCommit {
                references,
                ..commit_for(SP)
            },
        )
        .await
    }

    /// A resolved project enqueues a `cached_path_signature` placeholder for every
    /// subscribed cache. Regression guard: the detached NAR commit must resolve
    /// the project on the read loop before the job is evicted from the tracker,
    /// otherwise `SignTargets` collapses to `None`, no placeholder is written,
    /// the sign sweep has nothing to sign, and the narinfo 404s forever.
    #[tokio::test]
    async fn project_target_enqueues_signature_placeholder() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(false)])
            .append_query_results([vec![project_cache_row()]])
            .append_exec_results([exec(0), exec(0), exec(1), exec(1)])
            .into_connection();

        let log = commit_and_log(
            db,
            &NarCommit {
                targets: SignTargets::ProjectCaches(project()),
                ..commit_for(SP)
            },
        )
        .await;

        assert!(
            log.iter().any(|s| s.contains("cached_path_signature")),
            "ProjectCaches target must insert a cached_path_signature placeholder"
        );
    }

    #[tokio::test]
    async fn ingest_records_content_address() {
        let ca = "text:sha256:006vc8gixyrcynsx4lz1qxingl0mdja3l0xw1nl0j73isg37x944";
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(false)])
            .append_exec_results([exec(0), exec(0), exec(0)])
            .into_connection();

        let log = commit_and_log(
            db,
            &NarCommit {
                ca: Some(ca.to_owned()),
                ..commit_for(SP)
            },
        )
        .await;

        assert!(
            log.iter().any(|s| s.contains(ca)),
            "the content address must be written to cached_path"
        );
    }

    /// No resolvable project records the path but enqueues no signature, so the
    /// endpoint can distinguish "not yet signed" from "will never be signed".
    #[tokio::test]
    async fn none_target_enqueues_no_signature() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(false)])
            .append_exec_results([exec(0), exec(0), exec(0)])
            .into_connection();

        let log = commit_and_log(db, &commit_for(SP)).await;

        assert!(
            !log.iter().any(|s| s.contains("cached_path_signature")),
            "None target must not touch cached_path_signature"
        );
    }

    /// A committed path seeds its counter from its references and, when that
    /// makes a path that was not whole whole, ripples the flip forward to its
    /// referrers, all before the outputs are marked cached.
    #[tokio::test]
    async fn a_whole_commit_seeds_then_ripples_to_its_referrers() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(true)])
            .append_query_results([referrer_counts(DEP_HASH, 1)])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_exec_results([exec(0), exec(1), exec(0)])
            .into_connection();

        let log = commit_and_log(
            db,
            &NarCommit {
                references: vec!["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep".to_owned()],
                ..commit_for(SP)
            },
        )
        .await;

        let seed = log
            .iter()
            .position(|s| s.contains("SET missing_references = ("))
            .expect("seed runs");
        let ripple = log
            .iter()
            .position(|s| s.contains("missing_references - c.n"))
            .expect("ripple runs");
        let marks = log
            .iter()
            .position(|s| s.contains("is_cached"))
            .expect("outputs are marked");
        assert!(seed < ripple && ripple < marks, "{log:?}");
    }

    /// The NAR is where a built output's runtime references are learned: every
    /// reference with a producer becomes a runtime edge from the path's producer,
    /// and the demand those edges carry is recomputed over the producer at once.
    /// Without it the anchors the new edges reach wait a sweep interval for demand
    /// they already have, which is the whole point of learning them here.
    #[tokio::test]
    async fn a_commit_writes_the_runtime_edges_its_references_name() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([vec![producer_row()]])
            .append_query_results([vec![producer_row()]])
            .append_query_results([vec![demand_row()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(false)])
            .append_exec_results([
                exec(0),
                exec(1),
                exec(1),
                exec(0),
                exec(1),
                exec(1),
                exec(0),
            ])
            .into_connection();

        let log = commit_and_log(
            db,
            &NarCommit {
                references: vec![format!("{DEP_HASH}-dep")],
                ..commit_for(SP)
            },
        )
        .await;

        let edges = log
            .iter()
            .position(|s| {
                s.contains("INSERT INTO derivation_dependency (derivation, dependency, kind)")
            })
            .expect("the runtime edges are written");
        let demand = log
            .iter()
            .position(|s| s.contains("region(evaluation, derivation, builder) AS"))
            .expect("demand is recomputed over the producer");
        assert!(
            edges < demand,
            "the recompute must see the edges it walks: {log:?}"
        );
    }

    /// A NAR that makes its producer present advances the anchor side in the SAME
    /// transaction: the producer's counter is seeded, what became whole is offered
    /// to the fetchable mark, and the derivation whose own `.drv` this is is offered
    /// to promotion. Without this the counters only move on the next sweep, and a
    /// dependent waits a sweep interval for an input it already has.
    #[tokio::test]
    async fn a_whole_commit_advances_the_anchors_behind_the_paths_it_completed() {
        let producer = Uuid::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([vec![BTreeMap::from([(
                "derivation".to_owned(),
                Value::from(producer),
            )])]])
            .append_query_results([vec![BTreeMap::from([
                ("derivation".to_owned(), Value::from(producer)),
                ("was_whole".to_owned(), Value::from(false)),
                ("whole".to_owned(), Value::from(true)),
            ])]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![BTreeMap::from([(
                "id".to_owned(),
                Value::from(Uuid::now_v7()),
            )])]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(false)])
            .append_exec_results([exec(0), exec(1), exec(1), exec(1), exec(0)])
            .into_connection();

        let log = commit_and_log(db, &commit_for(SP)).await;

        let seed = log
            .iter()
            .position(|s| s.contains("SET missing_runtime_deps = x.n"))
            .expect("the producer's counter is seeded");
        let mark = log
            .iter()
            .position(|s| s.contains("SET fetchable = true"))
            .expect("what became whole is offered to the mark");
        let promote = log
            .iter()
            .position(|s| s.contains("SET status = 1"))
            .expect("the owner of the .drv is offered to promotion");
        assert!(
            seed < mark && mark < promote,
            "the anchor side follows the seed that produced its set: {log:?}"
        );
    }

    /// Re-pushing a path that was already whole must ripple nothing: the seed
    /// still reports "whole", but its referrers were decremented when it first
    /// became whole, and a second pass would drive them below zero, where no gate
    /// can ever see them as whole again.
    #[tokio::test]
    async fn a_recommit_of_an_already_whole_path_ripples_nothing() {
        let log = recommit_log(Vec::new()).await;
        assert!(
            !log.iter().any(|s| s.contains("missing_references - c.n")),
            "an already-whole path must not be rippled again: {log:?}"
        );
    }

    /// The pre-commit endpoint must be read under the row lock. A maintenance
    /// retire ripples the same counters from its own transaction, outside the
    /// graph actor, so an unlocked read lets a reverse ripple land between the
    /// read and the write: the commit would then see `(true, false)` and
    /// increment a referrer a second time for one loss, permanently. The row read
    /// is the second statement; the reference endpoints are locked before it.
    #[tokio::test]
    async fn the_pre_commit_endpoint_is_read_under_the_row_lock() {
        let log = recommit_log(Vec::new()).await;
        assert!(
            log[1].contains("FOR UPDATE"),
            "the row read that holds the endpoint must lock it: {log:?}"
        );
    }

    /// The seed counts the reference rows under its own snapshot holding no lock,
    /// and three maintenance retires delete those rows from their own transactions
    /// without ever seeing an edge this commit has not committed yet. So the
    /// endpoints are locked first, and before the referrer's own `FOR UPDATE`:
    /// references-before-referrer on both sides is what keeps a commit and a retire
    /// from an ABBA cycle, and the shared hash order is what keeps two of either
    /// from one.
    #[tokio::test]
    async fn the_reference_endpoints_are_locked_before_the_referrer_row() {
        let log = recommit_log(vec!["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep".to_owned()]).await;
        let endpoints = log
            .iter()
            .position(|s| s.contains("FOR SHARE"))
            .expect("the reference endpoints are locked");
        let referrer = log
            .iter()
            .position(|s| s.contains("FOR UPDATE"))
            .expect("the referrer's row is locked");

        assert!(endpoints < referrer, "{log:?}");
        assert!(
            log[endpoints].contains("ORDER BY hash"),
            "one hash-ordered acquisition: {log:?}"
        );
    }

    /// A pooled handle releases every lock at the end of the statement that took
    /// it, so a commit on one races the maintenance retires with nothing held.
    /// Neither the compiler nor a `MockDatabase` can tell the two handles apart,
    /// so the commit refuses the pooled one instead of silently racing.
    #[tokio::test]
    async fn a_pooled_commit_is_rejected_instead_of_racing_the_retires() {
        let (pooled, _pool) =
            ctx(MockDatabase::new(DatabaseBackend::Postgres).into_connection()).await;

        let err = commit(&pooled, &commit_for(SP))
            .await
            .expect_err("a pooled commit must not run");

        assert!(
            err.to_string().contains("must run inside a transaction"),
            "{err}"
        );
    }

    /// A commit that names a reference whose producer is not whole takes its OWN
    /// producer out of wholeness: the new runtime edge is a hole, the seed reports
    /// the loss, and every anchor left fetchable against it dispatches a build whose
    /// input nothing can provide. The forward-only half of this pair is the dead
    /// zone this project has paid for repeatedly.
    #[tokio::test]
    async fn a_commit_that_loses_wholeness_ripples_backward() {
        let producer = Uuid::now_v7();
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([
                vec![returned_cached_path(HASH)],
                vec![returned_cached_path(HASH)],
            ])
            .append_query_results([vec![BTreeMap::from([(
                "derivation".to_owned(),
                Value::from(producer),
            )])]])
            .append_query_results([vec![producer_row()]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([vec![BTreeMap::from([
                ("derivation".to_owned(), Value::from(producer)),
                ("was_whole".to_owned(), Value::from(true)),
                ("whole".to_owned(), Value::from(false)),
            ])]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(true)])
            .append_exec_results([
                exec(0),
                exec(1),
                exec(1),
                exec(0),
                exec(1),
                exec(1),
                exec(1),
                exec(1),
            ])
            .into_connection();

        let log = commit_and_log(
            db,
            &NarCommit {
                references: vec!["cccccccccccccccccccccccccccccccc-gone".to_owned()],
                ..commit_for(SP)
            },
        )
        .await;

        assert!(
            log.iter()
                .any(|s| s
                    .contains("INSERT INTO derivation_dependency (derivation, dependency, kind)")),
            "the hole is recorded as a runtime edge: {log:?}"
        );
        assert!(
            log.iter().any(|s| s.contains("SET fetchable = false")),
            "an anchor that stopped being whole must stop being fetchable: {log:?}"
        );
    }

    /// The index write is authoritative, not add-only. The same store path can be
    /// re-uploaded with a DIFFERENT reference set - an input-addressed path
    /// rebuilt non-deterministically keeps its hash while its closure moves - so
    /// a reference the report dropped has to go, and one it kept has to take the
    /// position the report gives it. `missing_references` is counted from this
    /// table by both the seed and `repair_counters_for`, so an edge left behind
    /// over-counts that path forever: the repair recomputes from the same stale
    /// row and can never disagree with it.
    #[tokio::test]
    async fn a_recommit_rewrites_the_index_to_the_reported_set() {
        let dep = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep";
        let log = recommit_log(vec![dep.to_owned()]).await;
        let sync = log
            .iter()
            .find(|s| s.contains("INSERT INTO cached_path_reference"))
            .expect("the reference index is written");

        assert!(
            sync.contains("DELETE FROM cached_path_reference r"),
            "a reference the report dropped must go: {sync}"
        );
        assert!(
            sync.contains("NOT EXISTS (SELECT 1 FROM reported n"),
            "the prune must key on the reported set, not on the whole table: {sync}"
        );
        assert!(
            !sync.contains("DO NOTHING") && sync.contains("DO UPDATE"),
            "add-only leaves a surviving reference at a stale position: {sync}"
        );
        assert!(
            sync.contains("IS DISTINCT FROM EXCLUDED.position"),
            "an unchanged re-ingest must write no row version: {sync}"
        );
        assert!(
            sync.contains("DISTINCT ON (t.tok)"),
            "DO UPDATE refuses one row twice, so duplicate tokens fold first: {sync}"
        );
        assert!(
            sync.contains(dep),
            "the reported set is the parameter: {sync}"
        );
    }

    /// A shrink all the way to zero references is the case an add-only write
    /// cannot express at all, and skipping the write for an empty report would
    /// keep every stale edge: the seed that follows counts whatever is left in
    /// the table, so the prune has to run even when there is nothing to insert.
    #[tokio::test]
    async fn a_report_with_no_references_still_prunes_the_index() {
        let log = recommit_log(Vec::new()).await;
        assert!(
            log.iter()
                .any(|s| s.contains("DELETE FROM cached_path_reference r")),
            "a shrink to zero references must still clear the index: {log:?}"
        );
    }

    /// A removed edge is only ever counted by its referrer, which is the path
    /// being committed, so the prune must precede the seed: the seed recomputes
    /// this row's counter absolutely from the pruned table, and the caller-held
    /// pre-commit endpoint turns the resulting flip into the ripple to this row's
    /// own referrers. Nothing else re-derives the counter, so a prune ordered
    /// after the seed would leave the dropped edge counted until a repair pass.
    #[tokio::test]
    async fn the_prune_precedes_the_seed_that_recounts_the_row() {
        let log = recommit_log(vec!["bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-dep".to_owned()]).await;
        let prune = log
            .iter()
            .position(|s| s.contains("DELETE FROM cached_path_reference r"))
            .expect("the index is pruned");
        let seed = log
            .iter()
            .position(|s| s.contains("SET missing_references = ("))
            .expect("the counter is seeded");

        assert!(prune < seed, "{log:?}");
    }

    #[tokio::test]
    async fn a_committed_path_backs_every_output_with_its_hash() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(false)])
            .append_exec_results([exec(0), exec(0), exec(2)])
            .into_connection();
        let (ctx, _pool) = ctx(db).await;

        let committed = commit_in_transaction(&ctx, &commit_for(SP)).await.unwrap();
        assert!(committed.created);
        assert_eq!(committed.outputs_marked, 2);
    }

    /// A relayed NAR on S3 is committed before its object exists, so the row
    /// must say so.
    #[tokio::test]
    async fn a_relayed_commit_on_s3_inserts_the_row_unconfirmed() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([Vec::<MCachedPath>::new()])
            .append_query_results([vec![returned_cached_path(HASH)]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(false)])
            .append_exec_results([exec(0), exec(0), exec(0)])
            .into_connection();

        let log = commit_and_raw_log(
            db,
            &NarCommit {
                confirmed: false,
                ..commit_for(SP)
            },
        )
        .await;

        let insert = log
            .iter()
            .find(|s| s.sql.starts_with("INSERT INTO \"cached_path\""))
            .expect("the insert");
        assert_eq!(
            bound(insert, "confirmed"),
            Value::Bool(Some(false)),
            "{insert:?}"
        );
    }

    /// New bytes under an old hash on S3 are unconfirmed again until uploaded.
    #[tokio::test]
    async fn a_recommit_with_new_bytes_takes_the_commits_confirmed_flag() {
        let existing = MCachedPath {
            confirmed: true,
            file_hash: Some("sha256:old".to_owned()),
            ..returned_cached_path(HASH)
        };
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_query_results([vec![existing.clone()], vec![existing]])
            .append_query_results([Vec::<BTreeMap<String, Value>>::new()])
            .append_query_results([seed_reply(true)])
            .append_exec_results([exec(0), exec(1), exec(1)])
            .into_connection();

        let log = commit_and_raw_log(
            db,
            &NarCommit {
                confirmed: false,
                ..commit_for(SP)
            },
        )
        .await;

        let update = log
            .iter()
            .find(|s| s.sql.starts_with("UPDATE \"cached_path\""))
            .expect("the update");
        assert_eq!(
            bound(update, "confirmed"),
            Value::Bool(Some(false)),
            "{update:?}"
        );
    }

    #[tokio::test]
    async fn confirm_updates_only_the_row_whose_bytes_are_still_the_uploaded_ones() {
        let db = MockDatabase::new(DatabaseBackend::Postgres)
            .append_exec_results([exec(0)])
            .into_connection();
        let (ctx, pool) = ctx(db).await;

        let confirmed = confirm(
            &ctx,
            &NarConfirm {
                hash: HASH.to_owned(),
                file_hash: "sha256:abc".to_owned(),
            },
        )
        .await
        .expect("confirm");
        drop(ctx);

        assert!(!confirmed, "no row matched the uploaded file hash");
        let log = raw_statements(pool);
        let update = log.first().expect("one statement");
        assert!(
            update
                .sql
                .starts_with("UPDATE \"cached_path\" SET \"confirmed\" = "),
            "{update:?}"
        );
        assert_eq!(
            bound(update, "confirmed"),
            Value::Bool(Some(true)),
            "{update:?}"
        );
        assert!(update.sql.contains("\"file_hash\" = "), "{update:?}");
        assert!(
            format!("{:?}", update.values).contains("Bool(Some(false))"),
            "only an unconfirmed row matches: {update:?}"
        );
    }
}
