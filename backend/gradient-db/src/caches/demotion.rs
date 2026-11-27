/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::build::BuildStatus;
use gradient_entity::cached_path::Model as MCachedPath;
use gradient_types::ids::{CachedPathId, DerivationBuildId, DerivationId};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

pub use crate::graph::runtime_can_start::StoreContentToRetire;

#[derive(Debug)]
pub struct MissingInputDiagnosis {
    pub content_variant: CachedPathId,
    pub nar_object_storage_key: String,
    pub outputs_total: usize,
    pub producer_build_statuses: Vec<gradient_entity::build::BuildStatus>,
}

pub async fn diagnose_missing_input<C: ConnectionTrait>(
    db: &C,
    content_variant: &MCachedPath,
) -> Result<MissingInputDiagnosis, sea_orm::DbErr> {
    use gradient_entity::derivation_build::{Column as CDB, Entity as EDB};
    use gradient_entity::derivation_output::{Column as CDO, Entity as EDO};

    let outputs = EDO::find()
        .filter(CDO::Hash.eq(&content_variant.hash))
        .all(db)
        .await?;
    let producer_drvs: Vec<DerivationId> = outputs.iter().map(|o| o.derivation).collect();

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
        content_variant: content_variant.id,
        nar_object_storage_key: content_variant.nar_object_storage_key.clone(),
        outputs_total: outputs.len(),
        producer_build_statuses,
    })
}

fn demoted_output(
    o: gradient_entity::derivation_output::Model,
) -> gradient_entity::derivation_output::ActiveModel {
    use sea_orm::{ActiveValue::Set, IntoActiveModel};

    let mut active = o.into_active_model();
    active.external_url = Set(None);
    active.verified_by_upstream = Set(None);
    active.nar_hash = Set(None);
    active.file_hash = Set(None);
    active.file_size = Set(None);
    active.references = Set(None);
    active.deriver = Set(None);
    active
}

/// `ObjectMissing` demotes a reported path only after a storage probe found the object gone.
/// `Always` is for an invalidation, which wants the object gone.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DemoteWhen {
    ObjectMissing,
    Always,
}

fn probe_before_demoting(when: DemoteWhen, has_producer: bool) -> bool {
    when == DemoteWhen::ObjectMissing || !has_producer
}

crate::sql! {
    CLEAR_CACHE_AVAILABLE_TRUST = "UPDATE derivation_build SET cache_available = false \
             WHERE derivation = ANY($1) AND cache_available",
        params = [DerivationIds(64)];
}

pub async fn object_exists_in_storage_or_error(
    ctx: &crate::DbContext,
    content_variant: &MCachedPath,
) -> bool {
    ctx.storage
        .nar_storage
        .exists(&content_variant.nar_object_storage_key)
        .await
        .unwrap_or(true)
}

/// Acquire the [`crate::graph::runtime_can_start::lock_cached_paths`] first. Clear `cache_available` under that lock before a concurrent retire can read that value. All writers follow the class order `cached_path`, then `derivation`, then `derivation_build`. A concurrent retire can deadlock with this path on any other order.
#[derive(Debug, Default)]
pub struct DemotedDerivationsAndUnreferencedObjectKeys {
    pub producers: Vec<DerivationId>,
    pub unreferenced_cached_path_object_storage_keys: Vec<String>,
}

pub async fn find_cached_path_for_store_hash_with_cache_signature<C: ConnectionTrait>(
    db: &C,
    cache: gradient_types::CacheId,
    hash: &str,
) -> Result<Option<gradient_entity::cached_path::Model>, sea_orm::DbErr> {
    use gradient_entity::cached_path::{Column as CCP, Entity as ECP};
    use gradient_entity::cached_path_signature::{Column as CSig, Entity as ESig};
    use sea_orm::QuerySelect;
    use sea_orm::sea_query::Query;

    ECP::find()
        .filter(CCP::Hash.eq(hash))
        .filter(
            CCP::Id.in_subquery(
                Query::select()
                    .column(CSig::CachedPath)
                    .from(ESig)
                    .and_where(CSig::Cache.eq(cache))
                    .to_owned(),
            ),
        )
        .limit(1)
        .one(db)
        .await
}

pub async fn demote_cached_output(
    ctx: &crate::DbContext,
    content: &StoreContentToRetire,
    when: DemoteWhen,
    object_present: bool,
) -> Result<DemotedDerivationsAndUnreferencedObjectKeys, sea_orm::DbErr> {
    use gradient_entity::derivation_output::{Column as CDO, Entity as EDO};
    use sea_orm::{ActiveModelTrait, TransactionTrait};

    let db = &ctx.worker_db;
    let hashes = content.store_hashes_of_touched_cached_paths(db).await?;
    let graph_hashes = content
        .store_hashes_without_cached_path_after_retire(db)
        .await?;
    let outputs = EDO::find()
        .filter(CDO::Hash.is_in(hashes.iter().cloned()))
        .all(db)
        .await?;
    let has_producer = !outputs.is_empty();

    // Nothing is deleted on uncertainty. A present object stays, a probe error keeps it and a
    // producerless input has nothing to rebuild it.
    if hashes.is_empty() || probe_before_demoting(when, has_producer) && object_present {
        return Ok(DemotedDerivationsAndUnreferencedObjectKeys::default());
    }

    let mut producers = Vec::with_capacity(outputs.len());
    for o in outputs {
        if o.hash
            .as_ref()
            .is_some_and(|hash| graph_hashes.contains(hash))
        {
            producers.push(o.derivation);
            demoted_output(o).update(db).await?;
        }
    }

    let txn = db.begin().await?;
    crate::graph::runtime_can_start::lock_cached_paths(&txn, &hashes).await?;
    crate::graph::walk_completeness::mark_derivations_not_walked_for_store_path_hashes(
        &txn,
        &graph_hashes,
    )
    .await?;
    let _shared_builds = crate::graph::can_start::lock_shared_builds(&txn, &producers).await?;
    if !producers.is_empty() {
        let ids: Vec<uuid::Uuid> = producers.iter().map(|d| d.into_inner()).collect();
        txn.execute_raw(CLEAR_CACHE_AVAILABLE_TRUST.bind([ids.into()]))
            .await?;
    }

    let mut retired = crate::graph::runtime_can_start::retire_outputs(&txn, content).await?;
    retired
        .transitions
        .extend(crate::graph::needed_outputs::forget_unneeded_outputs(&txn, &producers).await?);
    retired
        .transitions
        .extend(crate::graph::can_start::unpromote_ungated(&txn, &producers).await?);
    txn.commit().await?;

    let settled = crate::graph::can_start::update_and_settle_need(db, &producers).await?;
    retired.transitions.extend(settled.changes);
    crate::status::emit_transition_effects(ctx, &retired.transitions).await?;

    Ok(DemotedDerivationsAndUnreferencedObjectKeys {
        producers,
        unreferenced_cached_path_object_storage_keys: retired
            .unreferenced_cached_path_object_storage_keys,
    })
}

/// The parents keep their cached outputs. Their next walk names the orphan producer below
/// them again.
pub async fn unwalk_parents_of(
    ctx: &crate::DbContext,
    missing_hash: &str,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    let parents = parents_of_hash(&ctx.worker_db, missing_hash).await?;
    let changes = crate::graph::can_start::unwalk_derivations(ctx, &parents).await?;
    crate::status::emit_transition_effects(ctx, &changes).await?;

    Ok(parents)
}

pub(crate) fn unbacked_trusted_outputs_select() -> String {
    format!(
        r#"
    SELECT DISTINCT o.hash
    FROM derivation_output o
    JOIN derivation_build db ON db.derivation = o.derivation
    WHERE db.status IN ({terminal_success})
      AND o.name <> ALL(db.unneeded_outputs)
      AND {unavailable}
"#,
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
        unavailable =
            crate::graph::predicates::sql_condition_for_derivation_output_without_cached_path("o"),
    )
}

fn select_store_paths_of_uncached_needed_outputs_for_derivation_build_id_sql() -> String {
    format!(
        r#"
    SELECT coalesce('/nix/store/' || o.hash || '-' || o.package, o.name) AS store_path
    FROM derivation_output o
    JOIN derivation_build db ON db.derivation = o.derivation
    WHERE db.id = $1
      AND {needed_without_cached_path}
    ORDER BY 1
"#,
        needed_without_cached_path =
            crate::graph::predicates::sql_condition_for_needed_output_without_cached_path(
                "o", "db",
            ),
    )
}

crate::sql_fn! {
    STORE_PATHS_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_FOR_DERIVATION_BUILD =
        select_store_paths_of_uncached_needed_outputs_for_derivation_build_id_sql,
        params = [SharedBuildId];
}

pub async fn find_needed_output_paths_missing_from_store<C: ConnectionTrait>(
    db: &C,
    derivation_build: DerivationBuildId,
) -> Result<Vec<String>, sea_orm::DbErr> {
    Ok(db
        .query_all_raw(
            STORE_PATHS_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_FOR_DERIVATION_BUILD
                .bind([derivation_build.into_inner().into()]),
        )
        .await?
        .iter()
        .filter_map(|r| r.try_get::<String>("", "store_path").ok())
        .collect())
}

pub const MAX_OUTPUT_STORE_PATH_HASHES_DEMOTED_PER_CONSISTENCY_PASS: u64 = 64;

pub const MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS: usize = 500;

pub const DEMOTION_MIN_TIME_SINCE_LAST_SUCCESSFUL_BUILD: std::time::Duration =
    std::time::Duration::from_secs(24 * 60 * 60);

fn successful_build_attempt_outcome_numbers_sql_list() -> String {
    use gradient_entity::build_attempt::AttemptOutcome;

    format!(
        "{}, {}",
        i32::from(AttemptOutcome::Built),
        i32::from(AttemptOutcome::Substituted)
    )
}

/// Stored and needed outputs of completed builds make up the query result. Their reference closures
/// contain the outputs themselves and hold a store path with no stored copy and no pending build.
/// That closure can also contain a path that no build produced, such as a source path or an
/// upstream path. A rebuild can upload that path again from the worker store. A failed worker
/// prefetch can point at it. Cover all start closures in one recursive walk and expand a shared
/// path once. Follow reference edges back to the start outputs on a second walk, but only for a
/// path with no stored copy and no pending build.
fn store_path_hashes_of_needed_outputs_without_stored_copy_oldest_first_limit_sql() -> String {
    use gradient_entity::evaluation::EvaluationStatus;

    format!(
        r#"
    WITH RECURSIVE derivation_outputs_still_needed AS (
    SELECT o.derivation, o.name, o.hash
    FROM entry_point ep
    JOIN evaluation ev ON ev.id = ep.evaluation AND ev.status NOT IN ({terminal_evaluation})
    JOIN derivation_output o ON o.derivation = ep.derivation
    WHERE NOT EXISTS (SELECT 1 FROM build_job bj
                      WHERE bj.evaluation = ep.evaluation
                        AND bj.derivation = ep.derivation AND bj.aborted)
    UNION
    SELECT o.derivation, o.name, o.hash
    FROM derivation_build p
    JOIN derivation pw ON pw.id = p.derivation
    JOIN derivation_dependency e ON e.derivation = p.derivation AND e.kind IN (0, 2)
    JOIN derivation_output o ON o.derivation = e.dependency
     AND (e.dependency_outputs IS NULL OR o.name = ANY(e.dependency_outputs))
    WHERE p.wanted AND {open} AND ({builder}) AND {live}
    ),
    closure_start_needed_output_hashes_with_oldest_build_time AS MATERIALIZED (
    SELECT needed.hash, min(db.updated_at) AS oldest_completed_build_updated_at
    FROM derivation_outputs_still_needed needed
    JOIN derivation_build db ON db.derivation = needed.derivation
    WHERE db.status IN ({terminal_success})
      AND needed.hash IS NOT NULL
      AND needed.name <> ALL(db.unneeded_outputs)
      AND NOT EXISTS (SELECT 1 FROM build_attempt a
                      WHERE a.derivation_build = db.id AND a.outcome IN ({successful})
                      GROUP BY a.derivation_build
                      HAVING count(*) > 1
                         AND max(COALESCE(a.build_finished_at, a.created_at))
                             > (now() AT TIME ZONE 'UTC') - interval '{window_seconds} seconds')
    GROUP BY needed.hash
    ),
    all_reachable_store_paths(hash) AS (
        SELECT hash FROM closure_start_needed_output_hashes_with_oldest_build_time
        UNION
        SELECT {token_hash}
        FROM (SELECT unnest(string_to_array(stored_copy."references", ' ')) AS token
              FROM all_reachable_store_paths reached
              JOIN cached_path stored_copy ON stored_copy.hash = reached.hash) r
        WHERE r.token <> ''
    ),
    store_path_hashes_without_stored_copy_or_pending_build AS MATERIALIZED (
        SELECT reached.hash FROM all_reachable_store_paths reached
        WHERE NOT EXISTS (SELECT 1 FROM cached_path cp WHERE cp.hash = reached.hash)
          AND NOT EXISTS (SELECT 1 FROM derivation_output producer_output
                          JOIN derivation_build producer ON producer.derivation = producer_output.derivation
                          WHERE producer_output.hash = reached.hash AND {pending})
    ),
    reached_store_path_references AS MATERIALIZED (
        SELECT r.referencing_hash, {token_hash} AS referenced_hash
        FROM (SELECT stored_copy.hash AS referencing_hash,
                     unnest(string_to_array(stored_copy."references", ' ')) AS token
              FROM all_reachable_store_paths reached
              JOIN cached_path stored_copy ON stored_copy.hash = reached.hash
              WHERE EXISTS (SELECT 1 FROM store_path_hashes_without_stored_copy_or_pending_build)) r
        WHERE r.token <> ''
    ),
    store_path_hashes_referencing_missing_paths(hash) AS (
        SELECT hash FROM store_path_hashes_without_stored_copy_or_pending_build
        UNION
        SELECT edge.referencing_hash
        FROM store_path_hashes_referencing_missing_paths referenced
        JOIN reached_store_path_references edge ON edge.referenced_hash = referenced.hash
    )
    SELECT start.hash
    FROM closure_start_needed_output_hashes_with_oldest_build_time start
    JOIN store_path_hashes_referencing_missing_paths USING (hash)
    ORDER BY start.oldest_completed_build_updated_at, start.hash
    LIMIT $1
"#,
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
        successful = successful_build_attempt_outcome_numbers_sql_list(),
        window_seconds = DEMOTION_MIN_TIME_SINCE_LAST_SUCCESSFUL_BUILD.as_secs(),
        terminal_evaluation = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL),
        token_hash =
            crate::store_path_content_variant_trust::STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION,
        pending = crate::graph::predicates::blocks_evaluation_predicate("producer"),
        open = crate::graph::predicates::open_predicate("p"),
        builder = crate::graph::predicates::builder_predicate("p", "pw"),
        live = crate::graph::predicates::derivation_build_has_build_job_not_aborted_and_evaluation_not_finished_sql_predicate("p"),
    )
}

crate::sql_fn! {
    STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT =
        store_path_hashes_of_needed_outputs_without_stored_copy_oldest_first_limit_sql,
        params = [Int(64)],
        tier = Sweep;
}

fn completed_derivation_builds_with_more_than_one_successful_attempt_and_last_successful_attempt_worker_id_for_output_store_path_hashes_sql()
-> String {
    format!(
        r#"
    SELECT DISTINCT db.id AS derivation_build, o.hash,
           (SELECT dj.worker_id FROM build_attempt a
            LEFT JOIN dispatched_job dj ON dj.id = a.dispatched_job
            WHERE a.derivation_build = db.id AND a.outcome IN ({successful})
            ORDER BY COALESCE(a.build_finished_at, a.created_at) DESC
            LIMIT 1) AS worker_id
    FROM derivation_output o
    JOIN derivation_build db ON db.derivation = o.derivation
    WHERE o.hash = ANY($1::text[])
      AND db.status IN ({terminal_success})
      AND (SELECT count(*) FROM build_attempt a
           WHERE a.derivation_build = db.id AND a.outcome IN ({successful})) > 1
"#,
        successful = successful_build_attempt_outcome_numbers_sql_list(),
        terminal_success = crate::sql::status::build_in(&BuildStatus::TERMINAL_SUCCESS),
    )
}

crate::sql_fn! {
    COMPLETED_DERIVATION_BUILDS_WITH_MORE_THAN_ONE_SUCCESSFUL_ATTEMPT_AND_LAST_SUCCESSFUL_ATTEMPT_WORKER_ID_FOR_OUTPUT_STORE_PATH_HASHES =
        completed_derivation_builds_with_more_than_one_successful_attempt_and_last_successful_attempt_worker_id_for_output_store_path_hashes_sql,
        params = [CachedPathHashes(64)],
        tier = Sweep;
}

pub async fn demote_completed_builds_missing_needed_stored_output_copies(
    ctx: &crate::DbContext,
    bound: u64,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    let hashes: Vec<String> = ctx
        .worker_db
        .query_all_raw(
            STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT
                .bind([(bound as i64).into()]),
        )
        .await?
        .iter()
        .filter_map(|r| r.try_get::<String>("", "hash").ok())
        .collect();
    if hashes.is_empty() {
        return Ok(Vec::new());
    }

    let completed_build_rows_with_multiple_successful_attempts_to_demote_again = ctx
        .worker_db
        .query_all_raw(
            COMPLETED_DERIVATION_BUILDS_WITH_MORE_THAN_ONE_SUCCESSFUL_ATTEMPT_AND_LAST_SUCCESSFUL_ATTEMPT_WORKER_ID_FOR_OUTPUT_STORE_PATH_HASHES
                .bind([hashes.clone().into()]),
        )
        .await?;
    let producers = demote_cached_output(
        ctx,
        &StoreContentToRetire::StoreHashesWithGraphOutput(hashes),
        DemoteWhen::Always,
        false,
    )
    .await?
    .producers;

    for row in completed_build_rows_with_multiple_successful_attempts_to_demote_again {
        let derivation_build = row.try_get::<uuid::Uuid>("", "derivation_build").ok();
        let store_path_hash = row.try_get::<String>("", "hash").ok();
        let last_successful_attempt_worker_id = row
            .try_get::<Option<String>>("", "worker_id")
            .ok()
            .flatten();
        tracing::warn!(
            ?derivation_build,
            ?store_path_hash,
            ?last_successful_attempt_worker_id,
            "The periodic integrity check resubmitted a completed build for a second time. The build ran again, but the output never reached the store. Check the worker from the last successful attempt."
        );
    }

    Ok(producers)
}

crate::sql! {
    OUTPUT_PARENTS_SELECT = "SELECT DISTINCT e.derivation AS parent \
     FROM derivation_dependency e \
     WHERE e.kind IN (1, 2) \
       AND e.dependency IN (SELECT p.derivation FROM derivation_output p WHERE p.hash = $1)",
        params = [CachedPathHash];
}

async fn parents_of_hash<C: ConnectionTrait>(
    db: &C,
    hash: &str,
) -> Result<Vec<DerivationId>, sea_orm::DbErr> {
    use sea_orm::FromQueryResult;

    #[derive(sea_orm::FromQueryResult)]
    struct ParentRow {
        parent: uuid::Uuid,
    }

    Ok(
        ParentRow::find_by_statement(OUTPUT_PARENTS_SELECT.bind([hash.into()]))
            .all(db)
            .await?
            .into_iter()
            .map(|r| DerivationId::new(r.parent))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::{mock_db, statements};

    fn present_nar(hash: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nars").join(&hash[..2]);
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join(format!("{}.nar.zst", &hash[2..]));
        std::fs::write(&file, b"x").unwrap();
        (tmp, file)
    }

    #[tokio::test]
    async fn demote_preserves_a_present_producerless_object() {
        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, file) = present_nar(hash);

        let db = mock_db()
            .append_query_results([Vec::<gradient_entity::derivation_output::Model>::new()])
            .into_connection();
        let (ctx, _pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let stored = MCachedPath {
            hash: hash.into(),
            nar_object_storage_key: hash.into(),
            ..Default::default()
        };
        let object_present = object_exists_in_storage_or_error(&ctx, &stored).await;
        let producers = demote_cached_output(
            &ctx,
            &StoreContentToRetire::StoreHashesWithGraphOutput(vec![hash.to_owned()]),
            DemoteWhen::ObjectMissing,
            object_present,
        )
        .await
        .unwrap()
        .producers;

        assert!(producers.is_empty(), "a producerless input has no producer");
        assert!(
            file.exists(),
            "a present producerless object must be preserved"
        );
    }

    #[tokio::test]
    async fn demote_output_resets_producers_and_returns_storage_keys() {
        use gradient_entity::build::BuildStatus;
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;

        let [with_row, without_row, untouched] = ["1", "2", "3"].map(|c| c.repeat(32));
        let drv = |name: &str| format!("{}-{name}.drv", name.repeat(32));
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&drv("a"))
            .output(&with_row)
            .build_status(BuildStatus::Completed)
            .cached_path(&with_row, "a")
            .derivation(&drv("b"))
            .output(&without_row)
            .build_status(BuildStatus::Completed)
            .derivation(&drv("c"))
            .output(&untouched)
            .build_status(BuildStatus::Completed)
            .cached_path(&untouched, "c")
            .into_database()
            .await;
        let key = db
            .cached_path(&with_row)
            .await
            .unwrap()
            .nar_object_storage_key;
        let (a, b) = (db.id_of(&drv("a")).await, db.id_of(&drv("b")).await);
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _pool) = crate::test_ctx::ctx_at(db.connection(), tmp.path()).await;

        let demoted = demote_cached_output(
            &ctx,
            &StoreContentToRetire::StoreHashesWithGraphOutput(vec![
                with_row.clone(),
                without_row.clone(),
            ]),
            DemoteWhen::Always,
            true,
        )
        .await
        .unwrap();

        drop(ctx);
        let mut producers = demoted.producers.clone();
        producers.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(producers, expected);
        assert_eq!(demoted.unreferenced_cached_path_object_storage_keys, [key]);
        assert!(db.cached_path(&with_row).await.is_none());
        assert!(db.cached_path(&untouched).await.is_some());
        assert_ne!(
            db.shared_build(&drv("a")).await.status,
            BuildStatus::Completed
        );
        assert_ne!(
            db.shared_build(&drv("b")).await.status,
            BuildStatus::Completed
        );
        assert_eq!(
            db.shared_build(&drv("c")).await.status,
            BuildStatus::Completed
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn a_reported_missing_output_stays_while_its_object_is_present() {
        use gradient_types::ids::{DerivationId, DerivationOutputId};

        let hash = "bn1sgl0pn88d9dkc10jp0i1a77iadh8w";
        let (tmp, file) = present_nar(hash);
        let output = gradient_entity::derivation_output::Model {
            id: DerivationOutputId::now_v7(),
            derivation: DerivationId::now_v7(),
            hash: Some(hash.to_string()),
            ..Default::default()
        };
        let db = mock_db()
            .append_query_results([vec![output]])
            .into_connection();
        let (ctx, pool) = crate::test_ctx::ctx_at(db, tmp.path()).await;

        let stored = MCachedPath {
            hash: hash.into(),
            nar_object_storage_key: hash.into(),
            ..Default::default()
        };
        let object_present = object_exists_in_storage_or_error(&ctx, &stored).await;
        let producers = demote_cached_output(
            &ctx,
            &StoreContentToRetire::StoreHashesWithGraphOutput(vec![hash.to_owned()]),
            DemoteWhen::ObjectMissing,
            object_present,
        )
        .await
        .unwrap()
        .producers;

        assert!(
            producers.is_empty(),
            "nothing was demoted, so nothing is requeued"
        );
        assert!(
            file.exists(),
            "a worker's outdated view of the cache deletes no object"
        );
        drop(ctx);
        let log = statements(pool.into_transaction_log());
        assert_eq!(
            log.len(),
            1,
            "the outputs are read and the probe ends it, before any output is touched: {log:?}"
        );
    }

    #[test]
    fn demote_clears_upstream_availability() {
        use sea_orm::ActiveValue::Set;

        let o = gradient_entity::derivation_output::Model {
            external_url: Some("https://cache.example/x.narinfo".to_string()),
            nar_hash: Some("sha256:aaa".to_string()),
            file_hash: Some("sha256:bbb".to_string()),
            file_size: Some(42),
            ..Default::default()
        };

        let am = demoted_output(o);
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
    fn a_reported_missing_path_is_probed_and_an_invalidation_probes_only_producerless_paths() {
        assert!(probe_before_demoting(DemoteWhen::ObjectMissing, true));
        assert!(probe_before_demoting(DemoteWhen::ObjectMissing, false));
        assert!(
            !probe_before_demoting(DemoteWhen::Always, true),
            "an invalidated output is demoted so its producer rebuilds it"
        );
        assert!(
            probe_before_demoting(DemoteWhen::Always, false),
            "a producerless input has nothing to rebuild it, so a present copy is kept"
        );
    }

    #[tokio::test]
    async fn output_parents_are_the_runtime_dependents_of_the_missing_output() {
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;

        let dependency = format!("{}-dependency.drv", "d".repeat(32));
        let runtime = format!("{}-runtime.drv", "a".repeat(32));
        let buildtime = format!("{}-buildtime.drv", "b".repeat(32));
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_BUILD_OUTPUT_STORE_PATH_HASH)
            .derivation(&runtime)
            .runtime_depends_on(&dependency)
            .derivation(&buildtime)
            .depends_on(&dependency)
            .into_database()
            .await;

        let parents =
            parents_of_hash(&db.connection(), DEPENDENCY_BUILD_OUTPUT_STORE_PATH_HASH).await;
        let unknown = parents_of_hash(&db.connection(), &"z".repeat(32)).await;
        let runtime_id = db.id_of(&runtime).await;
        db.finish().await;

        assert_eq!(parents.unwrap(), vec![runtime_id]);
        assert!(unknown.unwrap().is_empty());
    }

    #[tokio::test]
    async fn diagnosis_names_the_content_variant_whose_object_is_missing() {
        use crate::store_path_content_variant_reader_choice::list_all_content_variant_rows_for_store_path_hashes;
        use gradient_fixtures::store_path_content_variants::{
            FIRST_KEY, SECOND_KEY, SHARED, two_variants_of_one_hash_and_one_untrusted_hash,
        };

        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let tmp = tempfile::tempdir().unwrap();
        let (ctx, _pool) = crate::test_ctx::ctx_at(readers.db.connection(), tmp.path()).await;
        ctx.storage
            .nar_storage
            .put(SECOND_KEY, b"x".to_vec())
            .await
            .unwrap();
        let variants =
            list_all_content_variant_rows_for_store_path_hashes(&ctx.worker_db, &[SHARED.into()])
                .await
                .unwrap();

        let mut probed = Vec::new();
        for variant in &variants {
            let diagnosis = diagnose_missing_input(&ctx.worker_db, variant)
                .await
                .unwrap();
            assert_eq!(diagnosis.content_variant, variant.id);
            let object_present = object_exists_in_storage_or_error(&ctx, variant).await;
            probed.push((diagnosis.nar_object_storage_key, object_present));
        }
        probed.sort();

        assert_eq!(
            probed,
            vec![(FIRST_KEY.to_owned(), false), (SECOND_KEY.to_owned(), true)]
        );
        drop(ctx);
        readers.db.finish().await;
    }

    const DEPENDENCY_BUILD_OUTPUT_STORE_PATH_HASH: &str = "88888888888888888888888888888888";
    const NIX_STORE_PATH_HASH_OF_STORE_PATH_R_REFERENCED_BY_STORED_INPUT_X: &str =
        "99999999999999999999999999999999";

    async fn seed_parent_job_depending_on_output_referencing_store_path_without_copy_and_optional_producer_and_return_selected_store_path_hashes(
        build_status_of_build_producing_referenced_store_path: Option<BuildStatus>,
        has_stored_copy: bool,
    ) -> Vec<String> {
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;

        let x = DEPENDENCY_BUILD_OUTPUT_STORE_PATH_HASH;
        let r = NIX_STORE_PATH_HASH_OF_STORE_PATH_R_REFERENCED_BY_STORED_INPUT_X;
        let mut seed = shape().evaluation(EvaluationId::now_v7());
        if let Some(status) = build_status_of_build_producing_referenced_store_path {
            seed = seed
                .derivation(&format!("{}-reference.drv", "r".repeat(32)))
                .output(r)
                .build_status(status)
                .shared_build_option(|b| b.wanted = true);
        }
        if has_stored_copy {
            seed = seed.cached_path(r, "reference");
        }
        seed = seed
            .derivation(&format!("{}-dependency.drv", "x".repeat(32)))
            .output(x)
            .build_status(BuildStatus::Completed)
            .cached_path(x, "dependency")
            .references(&[(r, "reference")]);
        if build_status_of_build_producing_referenced_store_path.is_some() {
            seed = seed
                .runtime_depends_on(&format!("{}-reference.drv", "r".repeat(32)))
                .shared_build_option(|b| b.missing_runtime_deps = i32::from(!has_stored_copy));
        }
        let db = seed
            .derivation(&format!("{}-parent.drv", "p".repeat(32)))
            .depends_on(&format!("{}-dependency.drv", "x".repeat(32)))
            .walked()
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .job()
            .into_database()
            .await;
        let hashes = db
            .connection()
            .query_all_raw(
                STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT.bind([
                    (MAX_OUTPUT_STORE_PATH_HASHES_DEMOTED_PER_CONSISTENCY_PASS as i64).into(),
                ]),
            )
            .await
            .unwrap()
            .iter()
            .filter_map(|row| row.try_get::<String>("", "hash").ok())
            .collect();
        db.finish().await;
        hashes
    }

    #[tokio::test]
    async fn demotion_candidate_query_returns_producer_output_hash_for_unstored_referenced_path_and_not_after_it_is_stored()
     {
        assert_eq!(
            seed_parent_job_depending_on_output_referencing_store_path_without_copy_and_optional_producer_and_return_selected_store_path_hashes(Some(BuildStatus::Completed), false).await,
            vec![DEPENDENCY_BUILD_OUTPUT_STORE_PATH_HASH.to_owned()],
            "The query can select the build output for stored input X. X can reference R with no stored copy. Another build can produce R."
        );
        assert_eq!(
            seed_parent_job_depending_on_output_referencing_store_path_without_copy_and_optional_producer_and_return_selected_store_path_hashes(Some(BuildStatus::Completed), true).await,
            Vec::<String>::new(),
            "The query selected nothing when a stored copy of R, a referenced store path, was present."
        );
    }

    #[tokio::test]
    async fn demotion_candidate_query_returns_output_when_input_store_path_has_no_stored_copy_and_no_build_produces_it()
     {
        assert_eq!(
            seed_parent_job_depending_on_output_referencing_store_path_without_copy_and_optional_producer_and_return_selected_store_path_hashes(None, false).await,
            vec![DEPENDENCY_BUILD_OUTPUT_STORE_PATH_HASH.to_owned()],
            "The query picked a stored output X even though the store path R it points to has no stored copy and no build ever produced R."
        );
    }

    #[tokio::test]
    async fn demotion_candidate_query_returns_nothing_when_referenced_store_path_is_pending_build_without_stored_copy()
     {
        assert_eq!(
            seed_parent_job_depending_on_output_referencing_store_path_without_copy_and_optional_producer_and_return_selected_store_path_hashes(Some(BuildStatus::Building), false).await,
            Vec::<String>::new(),
            "The query selected nothing. A build for R still had to run. That build remained pending. R is a referenced store path with no stored copy."
        );
    }

    const NIX_STORE_PATH_HASH_OF_STORE_PATH_Y_REFERENCED_BY_STORED_COPY_OF_BUILD_OUTPUT_X_AND_REFERENCING_R_WITHOUT_STORED_COPY: &str =
        "77777777777777777777777777777777";

    #[tokio::test]
    async fn demotion_candidate_query_returns_output_when_reference_two_levels_down_has_no_stored_copy_and_no_build_produces_it()
     {
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;

        let x = DEPENDENCY_BUILD_OUTPUT_STORE_PATH_HASH;
        let y = NIX_STORE_PATH_HASH_OF_STORE_PATH_Y_REFERENCED_BY_STORED_COPY_OF_BUILD_OUTPUT_X_AND_REFERENCING_R_WITHOUT_STORED_COPY;
        let r = NIX_STORE_PATH_HASH_OF_STORE_PATH_R_REFERENCED_BY_STORED_INPUT_X;
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .cached_path(y, "middle")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-dependency.drv", "x".repeat(32)))
            .output(x)
            .build_status(BuildStatus::Completed)
            .cached_path(x, "dependency")
            .references(&[(y, "middle")])
            .derivation(&format!("{}-parent.drv", "p".repeat(32)))
            .depends_on(&format!("{}-dependency.drv", "x".repeat(32)))
            .walked()
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .job()
            .into_database()
            .await;
        let hashes: Vec<String> = db
            .connection()
            .query_all_raw(
                STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT.bind([
                    (MAX_OUTPUT_STORE_PATH_HASHES_DEMOTED_PER_CONSISTENCY_PASS as i64).into(),
                ]),
            )
            .await
            .unwrap()
            .iter()
            .filter_map(|row| row.try_get::<String>("", "hash").ok())
            .collect();
        db.finish().await;
        assert_eq!(
            hashes,
            vec![x.to_owned()],
            "The query returned stored output X. X referenced Y and Y referenced R. R had no stored copy and no build produced it."
        );
    }

    fn store_path_hash_of_input_without_stored_reference_at_index(
        seeded_input_position_and_age_order: usize,
    ) -> String {
        format!("{seeded_input_position_and_age_order:0>32}")
    }

    async fn seed_open_wanted_parent_build_with_live_job_depending_on_older_stored_inputs_with_complete_reference_closure_and_newer_stored_inputs_referencing_store_paths_without_stored_copy_or_producing_build_run_demotion_candidate_query_with_row_limit_and_return_selected_store_path_hashes(
        older_input_count_with_all_references_stored: usize,
        newer_stored_inputs_referencing_store_path_without_copy_or_producing_build_count: usize,
        candidate_row_limit: i64,
    ) -> Vec<String> {
        use gradient_fixtures::shape::shape;
        use gradient_fixtures::test_date;
        use gradient_types::ids::EvaluationId;

        let store_path_hash_with_no_stored_copy_or_producing_build =
            NIX_STORE_PATH_HASH_OF_STORE_PATH_R_REFERENCED_BY_STORED_INPUT_X;
        let mut seed = shape().evaluation(EvaluationId::now_v7());
        let mut parent_build_seeded_input_derivation_paths = Vec::new();
        for seeded_input_position_and_age_order in 0..older_input_count_with_all_references_stored
            + newer_stored_inputs_referencing_store_path_without_copy_or_producing_build_count
        {
            let seeded_input_derivation_path = format!(
                "{seeded_input_position_and_age_order:a>32}-input-{seeded_input_position_and_age_order}.drv"
            );
            let output_store_path_hash_for_seeded_input = if seeded_input_position_and_age_order
                < older_input_count_with_all_references_stored
            {
                format!("{seeded_input_position_and_age_order:b>32}")
            } else {
                store_path_hash_of_input_without_stored_reference_at_index(
                    seeded_input_position_and_age_order,
                )
            };
            seed = seed
                .derivation(&seeded_input_derivation_path)
                .output(&output_store_path_hash_for_seeded_input)
                .build_status(BuildStatus::Completed)
                .shared_build_option(|b| {
                    b.updated_at = test_date()
                        + chrono::Duration::minutes(seeded_input_position_and_age_order as i64)
                })
                .cached_path(&output_store_path_hash_for_seeded_input, "input");
            if seeded_input_position_and_age_order >= older_input_count_with_all_references_stored {
                seed = seed.references(&[(
                    store_path_hash_with_no_stored_copy_or_producing_build,
                    "reference",
                )]);
            }
            parent_build_seeded_input_derivation_paths.push(seeded_input_derivation_path);
        }
        seed = seed.derivation(&format!("{}-parent.drv", "p".repeat(32)));
        for seeded_input_derivation_path in &parent_build_seeded_input_derivation_paths {
            seed = seed.depends_on(seeded_input_derivation_path);
        }
        let db = seed
            .walked()
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .job()
            .into_database()
            .await;
        let hashes = db
            .connection()
            .query_all_raw(
                STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT
                    .bind([candidate_row_limit.into()]),
            )
            .await
            .unwrap()
            .iter()
            .filter_map(|row| row.try_get::<String>("", "hash").ok())
            .collect();
        db.finish().await;
        hashes
    }

    #[tokio::test]
    async fn demotion_candidate_query_returns_oldest_input_with_missing_reference_copy_and_no_producing_build_beyond_row_limit()
     {
        assert_eq!(
            seed_open_wanted_parent_build_with_live_job_depending_on_older_stored_inputs_with_complete_reference_closure_and_newer_stored_inputs_referencing_store_paths_without_stored_copy_or_producing_build_run_demotion_candidate_query_with_row_limit_and_return_selected_store_path_hashes(3, 1, 2).await,
            vec![store_path_hash_of_input_without_stored_reference_at_index(3)],
            "The query returned a stored input with no stored copy in the reference and no origin build. Older inputs with full reference closure were more numerous and exceeded the row limit."
        );
        assert_eq!(
            seed_open_wanted_parent_build_with_live_job_depending_on_older_stored_inputs_with_complete_reference_closure_and_newer_stored_inputs_referencing_store_paths_without_stored_copy_or_producing_build_run_demotion_candidate_query_with_row_limit_and_return_selected_store_path_hashes(3, 2, 1).await,
            vec![store_path_hash_of_input_without_stored_reference_at_index(3)],
            "The query returned the older one with two such inputs and a line limit of 1."
        );
    }

    #[tokio::test]
    async fn select_returns_store_path_for_needed_output_without_stored_copy_and_skips_stored_and_unneeded_outputs()
     {
        use gradient_fixtures::shape::shape;
        use gradient_types::ids::EvaluationId;

        let stored = "11111111111111111111111111111111";
        let missing = "22222222222222222222222222222222";
        let unneeded = "33333333333333333333333333333333";
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&format!("{}-outputs.drv", "a".repeat(32)))
            .output(stored)
            .cached_path(stored, "outputs")
            .named_output(missing, "dev")
            .unneeded_output(unneeded, "doc")
            .build_status(BuildStatus::Building)
            .into_database()
            .await;
        let connection = db.connection();
        let derivation_build = gradient_entity::derivation_build::Entity::find()
            .one(&connection)
            .await
            .unwrap()
            .unwrap()
            .id;

        let paths = find_needed_output_paths_missing_from_store(&connection, derivation_build)
            .await
            .unwrap();

        assert!(
            paths.len() == 1 && paths[0].starts_with(&format!("/nix/store/{missing}-")),
            "The query returned only the needed outputs that have no stored copy. The query skipped stored outputs and unneeded outputs. {paths:?}"
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn migration_up_unwalks_source_listing_derivation_increments_parent_unwalked_inputs_and_selects_completed_output_for_demotion_from_null_hash_cached_paths()
     {
        use gradient_fixtures::shape::shape;
        use gradient_migration::{Migrator, MigratorTrait};
        use gradient_types::ids::EvaluationId;
        use sea_orm_migration::SchemaManager;

        let input_source_store_path_hash_with_null_hashes_deleted_by_migration = "4".repeat(32);
        let deleted_cached_path_output_store_path_hash = "5".repeat(32);
        let derivation_s_drv_path = format!("{}-source.drv", "c".repeat(32));
        let completed_build_derivation_path = format!("{}-build.drv", "b".repeat(32));
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&derivation_s_drv_path)
            .walked()
            .input_source(&format!(
                "/nix/store/{input_source_store_path_hash_with_null_hashes_deleted_by_migration}-source"
            ))
            .cached_path(
                &input_source_store_path_hash_with_null_hashes_deleted_by_migration,
                "source",
            )
            .derivation(&format!("{}-source-parent.drv", "d".repeat(32)))
            .depends_on(&derivation_s_drv_path)
            .walked()
            .derivation(&completed_build_derivation_path)
            .walked()
            .output(&deleted_cached_path_output_store_path_hash)
            .build_status(BuildStatus::Completed)
            .cached_path(&deleted_cached_path_output_store_path_hash, "build")
            .derivation(&format!("{}-build-parent.drv", "f".repeat(32)))
            .depends_on(&completed_build_derivation_path)
            .walked()
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .job()
            .into_database()
            .await;
        let connection = db.connection();
        let sql_reading_parent_walked_flag_and_unwalked_inputs_counter = format!(
            "SELECT walked::text || ' ' || unwalked_inputs::text FROM derivation WHERE hash = '{}'",
            "d".repeat(32)
        );
        let recount_changed_row_count =
            crate::graph::walk_completeness::recount_walk_completeness(&connection)
                .await
                .unwrap();
        let parent_walked_flag_and_unwalked_inputs_before_migration = db
            .texts(&sql_reading_parent_walked_flag_and_unwalked_inputs_counter)
            .await;
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20270102_000000_"))
            .expect("Register a migration to delete copies that have no stored bytes.");
        let manager = SchemaManager::new(&connection);
        migration
            .down(&manager)
            .await
            .expect("The migration rollback ran.");
        db.execute(&format!(
            "UPDATE cached_path SET file_hash = NULL, nar_hash = NULL \
             WHERE hash IN ('{input_source_store_path_hash_with_null_hashes_deleted_by_migration}', \
             '{deleted_cached_path_output_store_path_hash}')"
        ))
        .await;

        migration.up(&manager).await.expect("The migration ran.");
        let derivation_walked_flag_after_migration = db
            .texts(&format!(
                "SELECT walked::text FROM derivation WHERE hash = '{}'",
                "c".repeat(32)
            ))
            .await;
        let parent_derivation_walked_and_unwalked_inputs_after_migration = db
            .texts(&sql_reading_parent_walked_flag_and_unwalked_inputs_counter)
            .await;
        let walk_counter_recount_changed_row_count_after_migration_up =
            crate::graph::walk_completeness::recount_walk_completeness(&connection)
                .await
                .unwrap();
        let selected_demotion_candidate_store_path_hashes: Vec<String> = connection
            .query_all_raw(
                STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT.bind([
                    (MAX_OUTPUT_STORE_PATH_HASHES_DEMOTED_PER_CONSISTENCY_PASS as i64).into(),
                ]),
            )
            .await
            .unwrap()
            .iter()
            .filter_map(|row| row.try_get::<String>("", "hash").ok())
            .collect();
        let derivation_b_database_id = gradient_entity::derivation::Entity::find()
            .filter(gradient_entity::derivation::Column::Hash.eq("b".repeat(32)))
            .one(&connection)
            .await
            .unwrap()
            .expect("There is a derivation B.")
            .id;
        let completed_build_status_text_after_output_copy_deletion =
            gradient_entity::derivation_build::Entity::find()
                .filter(
                    gradient_entity::derivation_build::Column::Derivation
                        .eq(derivation_b_database_id),
                )
                .one(&connection)
                .await
                .unwrap()
                .expect("Build B is present.")
                .status;

        assert_eq!(
            recount_changed_row_count, 0,
            "Walk counters stay exact after seeding."
        );
        assert_eq!(
            derivation_walked_flag_after_migration,
            vec!["false".to_owned()],
            "Derivation S is not walked after migration. The input sources of derivation S had no bytes in the stored copy."
        );
        assert_eq!(
            (
                parent_walked_flag_and_unwalked_inputs_before_migration,
                parent_derivation_walked_and_unwalked_inputs_after_migration
            ),
            (vec!["true 0".to_owned()], vec!["true 1".to_owned()]),
            "Keep the parent of S walked and count one more unwalked input."
        );
        assert_eq!(
            walk_counter_recount_changed_row_count_after_migration_up, 0,
            "The walk counters stay accurate after the migration."
        );
        assert_eq!(
            selected_demotion_candidate_store_path_hashes,
            vec![deleted_cached_path_output_store_path_hash],
            "The query for demotion candidates selects the output of completed build B. The migration removed the stored copy of that output."
        );
        assert_eq!(
            completed_build_status_text_after_output_copy_deletion,
            BuildStatus::Completed,
            "Consistency passes keep Build B in Completed until they demote it."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn migration_up_sets_walked_derivation_to_unwalked_when_own_drv_cached_path_file_hash_is_null()
     {
        use gradient_fixtures::shape::shape;
        use gradient_migration::{Migrator, MigratorTrait};
        use gradient_types::ids::EvaluationId;
        use sea_orm_migration::SchemaManager;

        let derivation_e_store_hash = "h".repeat(32);
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&format!("{derivation_e_store_hash}-self.drv"))
            .walked()
            .cached_path(&derivation_e_store_hash, "self.drv")
            .into_database()
            .await;
        let connection = db.connection();
        let migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20270102_000000_"))
            .expect("Register a migration to delete copies that have no stored bytes.");
        let manager = SchemaManager::new(&connection);
        migration
            .down(&manager)
            .await
            .expect("The migration rollback ran.");
        db.execute(&format!(
            "UPDATE cached_path SET file_hash = NULL WHERE hash = '{derivation_e_store_hash}'"
        ))
        .await;

        migration.up(&manager).await.expect("The migration ran.");
        let derivation_e_walked_flag_rows_after_migration = db
            .texts(&format!(
                "SELECT walked::text FROM derivation WHERE hash = '{derivation_e_store_hash}'"
            ))
            .await;

        assert_eq!(
            derivation_e_walked_flag_rows_after_migration,
            vec!["false".to_owned()],
            "Derivation E remained unwalked after the migration. The stored copy of its own .drv file had no bytes."
        );
        db.finish().await;
    }

    const PREVIOUS_ALL_COMPLETED_OUTPUTS_DEMOTION_CANDIDATE_QUERY_SQL: &str = r#"
    WITH RECURSIVE closure_start_needed_output_hashes_with_oldest_build_time AS MATERIALIZED (
    SELECT o.hash, min(db.updated_at) AS oldest_completed_build_updated_at
    FROM derivation_output o
    JOIN derivation_build db ON db.derivation = o.derivation
    WHERE db.status IN (3, 7)
      AND o.hash IS NOT NULL
      AND o.name <> ALL(db.unneeded_outputs)
      AND NOT EXISTS (SELECT 1 FROM build_attempt a
                      WHERE a.derivation_build = db.id AND a.outcome IN (1, 2)
                      GROUP BY a.derivation_build
                      HAVING count(*) > 1
                         AND max(COALESCE(a.build_finished_at, a.created_at))
                             > (now() AT TIME ZONE 'UTC') - interval '86400 seconds')
      AND (EXISTS (SELECT 1 FROM entry_point ep
                   JOIN evaluation ev ON ev.id = ep.evaluation AND ev.status NOT IN (5, 6, 7)
                   WHERE ep.derivation = o.derivation
                     AND NOT EXISTS (SELECT 1 FROM build_job bj
                                     WHERE bj.evaluation = ep.evaluation
                                       AND bj.derivation = ep.derivation AND bj.aborted))
           OR EXISTS (SELECT 1 FROM derivation_dependency e
                      JOIN derivation_build p ON p.derivation = e.derivation
                      JOIN derivation pw ON pw.id = p.derivation
                      WHERE e.dependency = o.derivation AND e.kind IN (0, 2)
                        AND p.wanted AND (NOT p.fetchable AND p.status NOT IN (4, 6, 9)) AND (pw.walked AND p.probed AND NOT p.cache_available AND p.status IN (0, 1, 2, 8)) AND EXISTS (SELECT 1 FROM build_job live_job JOIN evaluation live_evaluation ON live_evaluation.id = live_job.evaluation JOIN task live_task ON live_task.id = live_evaluation.task WHERE live_job.derivation_build = p.id AND NOT live_job.aborted AND live_evaluation.status NOT IN (5, 6, 7))
                        AND (e.dependency_outputs IS NULL OR o.name = ANY(e.dependency_outputs))))
    GROUP BY o.hash
    ),
    all_reachable_store_paths(hash) AS (
        SELECT hash FROM closure_start_needed_output_hashes_with_oldest_build_time
        UNION
        SELECT split_part(regexp_replace(r.token, '^/nix/store/', ''), '-', 1)
        FROM (SELECT unnest(string_to_array(stored_copy."references", ' ')) AS token
              FROM all_reachable_store_paths reached
              JOIN cached_path stored_copy ON stored_copy.hash = reached.hash) r
        WHERE r.token <> ''
    ),
    store_path_hashes_without_stored_copy_or_pending_build AS MATERIALIZED (
        SELECT reached.hash FROM all_reachable_store_paths reached
        WHERE NOT EXISTS (SELECT 1 FROM cached_path cp WHERE cp.hash = reached.hash)
          AND NOT EXISTS (SELECT 1 FROM derivation_output producer_output
                          JOIN derivation_build producer ON producer.derivation = producer_output.derivation
                          WHERE producer_output.hash = reached.hash AND (producer.status IN (0, 1, 2, 8, 10) AND (producer.wanted OR producer.status IN (1, 2))))
    ),
    reached_store_path_references AS MATERIALIZED (
        SELECT r.referencing_hash, split_part(regexp_replace(r.token, '^/nix/store/', ''), '-', 1) AS referenced_hash
        FROM (SELECT stored_copy.hash AS referencing_hash,
                     unnest(string_to_array(stored_copy."references", ' ')) AS token
              FROM all_reachable_store_paths reached
              JOIN cached_path stored_copy ON stored_copy.hash = reached.hash
              WHERE EXISTS (SELECT 1 FROM store_path_hashes_without_stored_copy_or_pending_build)) r
        WHERE r.token <> ''
    ),
    store_path_hashes_referencing_missing_paths(hash) AS (
        SELECT hash FROM store_path_hashes_without_stored_copy_or_pending_build
        UNION
        SELECT edge.referencing_hash
        FROM store_path_hashes_referencing_missing_paths referenced
        JOIN reached_store_path_references edge ON edge.referenced_hash = referenced.hash
    )
    SELECT start.hash
    FROM closure_start_needed_output_hashes_with_oldest_build_time start
    JOIN store_path_hashes_referencing_missing_paths USING (hash)
    ORDER BY start.oldest_completed_build_updated_at, start.hash
    LIMIT $1
"#;

    async fn assert_demotion_candidate_query_matches_previous_sql_at_each_row_limit_and_return_largest_limit_row_count(
        db: &gradient_fixtures::database::TestDatabase,
        row_limits: &[i64],
    ) -> usize {
        let connection = db.connection();
        let mut largest_limit_row_count = 0;
        for &row_limit in row_limits {
            let previous: Vec<String> = connection
                .query_all_raw(
                    STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT
                        .bind_built(
                            PREVIOUS_ALL_COMPLETED_OUTPUTS_DEMOTION_CANDIDATE_QUERY_SQL,
                            [row_limit.into()],
                        ),
                )
                .await
                .unwrap()
                .iter()
                .filter_map(|row| row.try_get::<String>("", "hash").ok())
                .collect();
            let new: Vec<String> = connection
                .query_all_raw(
                    STORE_PATH_HASHES_OF_NEEDED_OUTPUTS_WITHOUT_STORED_COPY_OLDEST_FIRST_LIMIT
                        .bind([row_limit.into()]),
                )
                .await
                .unwrap()
                .iter()
                .filter_map(|row| row.try_get::<String>("", "hash").ok())
                .collect();
            assert_eq!(
                new, previous,
                "The rewritten query for demotion candidates returned a different store path hash or a different order from the previous SQL text at row limit {row_limit}."
            );
            largest_limit_row_count = previous.len();
        }
        assert!(
            largest_limit_row_count > 0,
            "The previous SQL text returned no rows on this fixture graph, so the comparison can't prove anything."
        );
        largest_limit_row_count
    }

    #[tokio::test]
    async fn demotion_candidate_query_previous_and_new_sql_text_return_same_store_path_hashes_in_same_order()
     {
        use gradient_entity::build_attempt::AttemptOutcome;
        use gradient_entity::evaluation::EvaluationStatus;
        use gradient_fixtures::shape::shape;
        use gradient_fixtures::test_date;
        use gradient_types::ids::EvaluationId;

        let r = NIX_STORE_PATH_HASH_OF_STORE_PATH_R_REFERENCED_BY_STORED_INPUT_X;
        let x = DEPENDENCY_BUILD_OUTPUT_STORE_PATH_HASH;
        let unneeded = "1".repeat(32);
        let idle = "2".repeat(32);
        let null_hash = "3".repeat(32);
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&format!("{}-dependency.drv", "x".repeat(32)))
            .output(x)
            .build_status(BuildStatus::Completed)
            .cached_path(x, "dependency")
            .references(&[(r, "reference")])
            .unneeded_output(&unneeded, "doc")
            .cached_path(&unneeded, "doc")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-null-hash.drv", "n".repeat(32)))
            .output(&null_hash)
            .build_status(BuildStatus::Completed)
            .cached_path(&null_hash, "null-hash")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-idle-dependency.drv", "z".repeat(32)))
            .output(&idle)
            .build_status(BuildStatus::Completed)
            .cached_path(&idle, "idle")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-idle-parent.drv", "q".repeat(32)))
            .depends_on(&format!("{}-idle-dependency.drv", "z".repeat(32)))
            .walked()
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .derivation(&format!("{}-parent.drv", "p".repeat(32)))
            .depends_on(&format!("{}-dependency.drv", "x".repeat(32)))
            .depends_on(&format!("{}-null-hash.drv", "n".repeat(32)))
            .walked()
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .job()
            .into_database()
            .await;
        db.execute(&format!(
            "UPDATE derivation_output SET hash = NULL WHERE hash = '{null_hash}'"
        ))
        .await;
        let needed_output_without_stored_reference_row_count =
            assert_demotion_candidate_query_matches_previous_sql_at_each_row_limit_and_return_largest_limit_row_count(&db, &[1, 64]).await;
        db.finish().await;

        let closed_evaluation = EvaluationId::now_v7();
        let live_evaluation = EvaluationId::now_v7();
        let db = shape()
            .evaluation(closed_evaluation)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(&format!("{}-closed-entry.drv", "c".repeat(32)))
            .output(&"4".repeat(32))
            .build_status(BuildStatus::Completed)
            .cached_path(&"4".repeat(32), "closed")
            .references(&[(r, "reference")])
            .entry_point("closed")
            .evaluation(live_evaluation)
            .derivation(&format!("{}-live-entry.drv", "l".repeat(32)))
            .output(&"5".repeat(32))
            .build_status(BuildStatus::Completed)
            .cached_path(&"5".repeat(32), "live")
            .references(&[(r, "reference")])
            .entry_point("live")
            .derivation(&format!("{}-aborted-entry.drv", "k".repeat(32)))
            .output(&"6".repeat(32))
            .build_status(BuildStatus::Completed)
            .cached_path(&"6".repeat(32), "aborted")
            .references(&[(r, "reference")])
            .entry_point("aborted")
            .job()
            .job_option(|j| j.aborted = true)
            .into_database()
            .await;
        let closed_and_live_evaluation_row_count =
            assert_demotion_candidate_query_matches_previous_sql_at_each_row_limit_and_return_largest_limit_row_count(&db, &[1, 64]).await;
        db.finish().await;

        let mut seed = shape().evaluation(EvaluationId::now_v7());
        for input in 0..4usize {
            seed = seed
                .derivation(&format!("{input:a>32}-input-{input}.drv"))
                .output(&format!("{input:b>32}"))
                .build_status(BuildStatus::Completed)
                .shared_build_option(|b| {
                    b.updated_at = test_date() + chrono::Duration::minutes(4 - input as i64)
                })
                .cached_path(&format!("{input:b>32}"), "input")
                .references(&[(r, "reference")]);
            if input == 0 {
                seed = seed.entry_point("input");
            }
            if input == 1 {
                seed = seed
                    .named_output(&format!("{input:d>32}"), "dev")
                    .cached_path(&format!("{input:d>32}"), "dev")
                    .references(&[(r, "reference")]);
            }
        }
        seed = seed
            .derivation(&format!("{}-entry.drv", "m".repeat(32)))
            .output(&"7".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date())
            .cached_path(&"7".repeat(32), "entry")
            .references(&[(r, "reference")])
            .entry_point("entry")
            .derivation(&format!("{}-parent.drv", "p".repeat(32)))
            .depends_on(&format!("{:a>32}-input-0.drv", 0))
            .depends_on_outputs(&format!("{:a>32}-input-1.drv", 1), &["out"])
            .depends_on(&format!("{:a>32}-input-2.drv", 2))
            .depends_on(&format!("{:a>32}-input-3.drv", 3));
        let db = seed
            .walked()
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .job()
            .into_database()
            .await;
        let more_candidates_than_row_limit_row_count =
            assert_demotion_candidate_query_matches_previous_sql_at_each_row_limit_and_return_largest_limit_row_count(&db, &[1, 2, 3, 64]).await;
        db.finish().await;

        let aborted_build_evaluation_id = EvaluationId::now_v7();
        let db = shape()
            .evaluation(closed_evaluation)
            .evaluation_status(EvaluationStatus::Completed)
            .derivation(&format!("{}-entry-in-three.drv", "a".repeat(32)))
            .output(&"6".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(20))
            .cached_path(&"6".repeat(32), "entry-in-three")
            .references(&[(r, "reference")])
            .entry_point("three")
            .evaluation(aborted_build_evaluation_id)
            .derivation(&format!("{}-entry-in-three.drv", "a".repeat(32)))
            .entry_point("three")
            .job()
            .job_option(|j| j.aborted = true)
            .evaluation(live_evaluation)
            .derivation(&format!("{}-entry-in-three.drv", "a".repeat(32)))
            .entry_point("three")
            .derivation(&format!("{}-cached-parent-input.drv", "b".repeat(32)))
            .output(&"1".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(2))
            .cached_path(&"1".repeat(32), "cached-parent-input")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-cached-parent.drv", "c".repeat(32)))
            .depends_on(&format!("{}-cached-parent-input.drv", "b".repeat(32)))
            .walked()
            .probed()
            .cache_available()
            .shared_build_option(|b| b.wanted = true)
            .job()
            .derivation(&format!("{}-unwanted-parent-input.drv", "d".repeat(32)))
            .output(&"4".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(3))
            .cached_path(&"4".repeat(32), "unwanted-parent-input")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-unwanted-parent.drv", "f".repeat(32)))
            .depends_on(&format!("{}-unwanted-parent-input.drv", "d".repeat(32)))
            .walked()
            .probed()
            .job()
            .derivation(&format!("{}-both-input.drv", "g".repeat(32)))
            .output(&"2".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(12))
            .cached_path(&"2".repeat(32), "both-input")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-runtime-input.drv", "h".repeat(32)))
            .output(&"3".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(4))
            .cached_path(&"3".repeat(32), "runtime-input")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-unneeded-same-output.drv", "i".repeat(32)))
            .output(&"5".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date())
            .cached_path(&"5".repeat(32), "same-output")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-needed-same-output.drv", "j".repeat(32)))
            .output(&"5".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(10))
            .derivation(&format!("{}-later-same-output.drv", "k".repeat(32)))
            .output(&"5".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(15))
            .derivation(&format!("{}-rebuilt-input.drv", "l".repeat(32)))
            .output(&"7".repeat(32))
            .build_status(BuildStatus::Completed)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(1))
            .cached_path(&"7".repeat(32), "rebuilt-input")
            .references(&[(r, "reference")])
            .attempt(AttemptOutcome::Built, None)
            .attempt(AttemptOutcome::Built, None)
            .derivation(&format!("{}-substituted-input.drv", "m".repeat(32)))
            .output(&"8".repeat(32))
            .build_status(BuildStatus::Substituted)
            .shared_build_option(|b| b.updated_at = test_date() + chrono::Duration::minutes(5))
            .cached_path(&"8".repeat(32), "substituted-input")
            .references(&[(r, "reference")])
            .derivation(&format!("{}-builder-parent.drv", "n".repeat(32)))
            .both_depends_on(&format!("{}-both-input.drv", "g".repeat(32)))
            .runtime_depends_on(&format!("{}-runtime-input.drv", "h".repeat(32)))
            .depends_on(&format!("{}-needed-same-output.drv", "j".repeat(32)))
            .depends_on(&format!("{}-later-same-output.drv", "k".repeat(32)))
            .depends_on(&format!("{}-rebuilt-input.drv", "l".repeat(32)))
            .depends_on(&format!("{}-substituted-input.drv", "m".repeat(32)))
            .walked()
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .job()
            .into_database()
            .await;
        db.execute("UPDATE build_attempt SET build_finished_at = now() AT TIME ZONE 'UTC'")
            .await;
        let both_sql_texts_row_count_at_largest_row_limit =
            assert_demotion_candidate_query_matches_previous_sql_at_each_row_limit_and_return_largest_limit_row_count(&db, &[1, 2, 3, 4, 64]).await;
        db.finish().await;

        assert_eq!(
            (
                needed_output_without_stored_reference_row_count,
                closed_and_live_evaluation_row_count,
                more_candidates_than_row_limit_row_count,
                both_sql_texts_row_count_at_largest_row_limit,
            ),
            (1, 1, 5, 4)
        );
    }
}
