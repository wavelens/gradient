/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

//! All reads of stored content pick their content variant through this module. Readers that trust
//! several variants of one store path hash use the SQL expression `variant_rank` to order them.
//! Uploaded to the read cache first, built by a worker next, then the oldest row, then the smallest
//! ID.

use crate::graph::predicates::{
    content_variant_trusted_by_all_projects_with_live_job_on_shared_build,
    derivation_build_has_build_job_not_aborted_and_evaluation_not_finished_sql_predicate,
    stored_copy_in_input_closure_or_trusted_by_every_project_with_live_job_on_shared_build_sql_condition,
};
use crate::store_path_content_variant_trust::{
    project_trusts_content_variant_exists_sql, query_input_closure_store_path_hashes_for_build,
    variant_rank,
};
use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, Statement, Value};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const MIN_INPUT_CLOSURE_SET_RECOMPUTE_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Debug)]
struct StorePathHashSetAndLastRecomputeInstant {
    current_input_closure_store_path_hashes: Arc<HashSet<String>>,
    last_recompute_after_miss_instant: Option<Instant>,
}

#[derive(Debug, Clone, Default)]
pub struct StorePathInputClosureHashes(Arc<Mutex<Option<StorePathHashSetAndLastRecomputeInstant>>>);

impl StorePathInputClosureHashes {
    pub async fn input_closure_store_path_hashes<C: ConnectionTrait>(
        &self,
        db: &C,
        shared_build: DerivationBuildId,
    ) -> Result<Arc<HashSet<String>>, DbErr> {
        let mut input_closure_set_cache_guard = self.0.lock().await;
        if let Some(cached_input_closure_set_and_last_recompute_instant) =
            input_closure_set_cache_guard.as_ref()
        {
            return Ok(cached_input_closure_set_and_last_recompute_instant
                .current_input_closure_store_path_hashes
                .clone());
        }
        store_walk_result_as_current_path_set(
            &mut input_closure_set_cache_guard,
            db,
            shared_build,
            None,
        )
        .await
    }

    async fn refresh_input_closure_store_path_hashes_if_stale<C: ConnectionTrait>(
        &self,
        db: &C,
        shared_build: DerivationBuildId,
        current_read_instant: Instant,
    ) -> Result<Arc<HashSet<String>>, DbErr> {
        let mut input_closure_set_cache_guard = self.0.lock().await;
        if let Some(cached_input_closure_set_and_last_recompute_instant) =
            input_closure_set_cache_guard.as_ref()
            && let Some(last_recompute_instant) =
                cached_input_closure_set_and_last_recompute_instant
                    .last_recompute_after_miss_instant
            && current_read_instant.saturating_duration_since(last_recompute_instant)
                < MIN_INPUT_CLOSURE_SET_RECOMPUTE_INTERVAL
        {
            return Ok(cached_input_closure_set_and_last_recompute_instant
                .current_input_closure_store_path_hashes
                .clone());
        }
        store_walk_result_as_current_path_set(
            &mut input_closure_set_cache_guard,
            db,
            shared_build,
            Some(current_read_instant),
        )
        .await
    }

    async fn load_exec_job_program_closure_store_path_hashes<C: ConnectionTrait>(
        &self,
        db: &C,
        program: &str,
    ) -> Result<Arc<HashSet<String>>, DbErr> {
        let mut program_closure_set_cache_guard = self.0.lock().await;
        if let Some(cached_program_closure_set) = program_closure_set_cache_guard.as_ref() {
            return Ok(cached_program_closure_set
                .current_input_closure_store_path_hashes
                .clone());
        }
        let program_closure_store_path_hashes = Arc::new(
            crate::exec_jobs::closure::load_exec_job_program_closure_store_path_hashes_from_database(
                db, program,
            )
            .await?,
        );
        *program_closure_set_cache_guard = Some(StorePathHashSetAndLastRecomputeInstant {
            current_input_closure_store_path_hashes: program_closure_store_path_hashes.clone(),
            last_recompute_after_miss_instant: None,
        });
        Ok(program_closure_store_path_hashes)
    }

    pub fn shares_underlying_set(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

async fn store_walk_result_as_current_path_set<C: ConnectionTrait>(
    cached_input_closure_set: &mut Option<StorePathHashSetAndLastRecomputeInstant>,
    db: &C,
    shared_build: DerivationBuildId,
    last_recompute_after_miss_instant: Option<Instant>,
) -> Result<Arc<HashSet<String>>, DbErr> {
    let shared_build_input_closure_store_path_hashes =
        Arc::new(query_input_closure_store_path_hashes_for_build(db, shared_build).await?);
    *cached_input_closure_set = Some(StorePathHashSetAndLastRecomputeInstant {
        current_input_closure_store_path_hashes: shared_build_input_closure_store_path_hashes
            .clone(),
        last_recompute_after_miss_instant,
    });
    Ok(shared_build_input_closure_store_path_hashes)
}

#[derive(Debug, Clone)]
pub enum StorePathContentVariantReader {
    Project(ProjectId),
    SharedBuild(DerivationBuildId, StorePathInputClosureHashes),
    ExecJob(ProjectId, String, StorePathInputClosureHashes),
    Cache(CacheId),
}

const CHOSEN: &str = "reader_choice";
const NO_READING_CACHE: &str = "NULL::uuid";

pub fn cached_path_by_store_hash_subquery_sql(hash: &str, cache: &str) -> String {
    format!(
        "SELECT {CHOSEN}.* FROM cached_path {CHOSEN} \
         WHERE {CHOSEN}.hash = {hash} \
           AND EXISTS (SELECT 1 FROM cached_path_signature served_signature \
                       JOIN cache served_cache ON served_cache.id = served_signature.cache \
                       WHERE served_signature.cached_path = {CHOSEN}.id \
                         AND served_signature.cache = {cache} AND served_cache.active \
                         AND coalesce(served_signature.signature, \
                                      served_signature.previous_signing_key_signature) IS NOT NULL) \
         ORDER BY {rank} LIMIT 1",
        rank = variant_rank(CHOSEN, cache),
    )
}

pub fn single_cached_path_sql_for_project_and_store_hash(hash: &str, project: &str) -> String {
    trusted_variant_sql(
        hash,
        &project_trusts_content_variant_exists_sql(&format!("{CHOSEN}.id"), project),
    )
}

pub fn cached_path_subquery_for_store_hash(hash: &str) -> String {
    trusted_variant_sql(hash, "true")
}

pub fn cached_path_subquery_for_store_hash_with_input_closure(
    hash: &str,
    shared_build: &str,
    input_closure_hashes: &str,
) -> String {
    trusted_variant_sql(
        hash,
        &stored_copy_in_input_closure_or_trusted_by_every_project_with_live_job_on_shared_build_sql_condition(
            &format!("{CHOSEN}.id"),
            shared_build,
            &format!("{CHOSEN}.hash = ANY({input_closure_hashes})"),
        ),
    )
}

fn stored_copies_for_store_hash_usable_by_exec_job_subquery(
    hash: &str,
    project: &str,
    program_closure_hashes: &str,
) -> String {
    trusted_variant_sql(
        hash,
        &format!(
            "({CHOSEN}.hash = ANY({program_closure_hashes}) OR {trusted})",
            trusted = project_trusts_content_variant_exists_sql(&format!("{CHOSEN}.id"), project),
        ),
    )
}

pub fn select_best_stored_copy_for_own_build_outputs_trusted_by_all_projects_with_live_jobs_excluding_direct_use(
    hash: &str,
    shared_build: &str,
) -> String {
    trusted_variant_sql(
        hash,
        &content_variant_trusted_by_all_projects_with_live_job_on_shared_build(
            &format!("{CHOSEN}.id"),
            shared_build,
        ),
    )
}

fn select_derivation_id_and_chosen_stored_copy_row_for_unnested_derivation_store_path_hash_pairs_using_per_hash_subquery_on_shared_build(
    choice: &str,
) -> String {
    format!(
        "SELECT wanted.derivation AS reader_derivation, chosen.* \
         FROM unnest($1::uuid[], $2::text[]) wanted(derivation, hash) \
         JOIN derivation_build reader_build ON reader_build.derivation = wanted.derivation \
         CROSS JOIN LATERAL ({choice}) chosen"
    )
}

fn trusted_variant_sql(hash: &str, trusted: &str) -> String {
    build_trusted_variants_in_rank_order_sql_subquery(hash, trusted, "1")
}

fn build_trusted_variants_in_rank_order_sql_subquery(
    hash: &str,
    trusted: &str,
    limit: &str,
) -> String {
    format!(
        "SELECT {CHOSEN}.* FROM cached_path {CHOSEN} \
         WHERE {CHOSEN}.hash = {hash} AND {trusted} \
         ORDER BY {rank} LIMIT {limit}",
        rank = variant_rank(CHOSEN, NO_READING_CACHE),
    )
}

pub const RAISED_QUERY_BUDGET_REASON_FOR_TRUSTED_OUTPUT_CHECK_AND_STORED_COPY_READER_BECAUSE_EVERY_LIVE_BUILD_JOB_TRIGGERS_TRUST_CHECK: &str =
    "Each live build job in the derivation process can trigger the trust check of its project. \
     These statements read 3758 and 3568 buffers on an amplified dump with 1000 unfinished evaluations. \
     The text before this change read 2854 and up to 23998 buffers.";

fn per_wanted_hash(choice: &str, reader_join: &str) -> String {
    format!(
        "SELECT chosen.* FROM unnest($1::text[]) wanted(hash) {reader_join} \
         CROSS JOIN LATERAL ({choice}) chosen"
    )
}

crate::sql_fn! {
    CONTENT_VARIANT_ROWS_SERVED_BY_BINARY_CACHE_FOR_STORE_HASHES_QUERY = || per_wanted_hash(
        &cached_path_by_store_hash_subquery_sql("wanted.hash", "$2::uuid"),
        "",
    ),
        params = [CachedPathHashes(64), CacheId];

    TRUSTED_CONTENT_VARIANT_PER_STORE_HASH_FOR_PROJECT_QUERY = || per_wanted_hash(
        &single_cached_path_sql_for_project_and_store_hash("wanted.hash", "$2::uuid"),
        "",
    ),
        params = [CachedPathHashes(64), ProjectId];

    TRUSTED_CONTENT_VARIANTS_FOR_PROJECT_STORE_PATH_HASH_IN_RANK_ORDER_LIMIT_N_QUERY = || {
        build_trusted_variants_in_rank_order_sql_subquery(
            "$1",
            &project_trusts_content_variant_exists_sql(&format!("{CHOSEN}.id"), "$2::uuid"),
            "$3",
        )
    },
        params = [CachedPathHash, ProjectId, Int(4)];

    STORED_COPY_BY_STORE_PATH_HASH_USABLE_AS_SHARED_BUILD_BUILD_INPUT_QUERY = || per_wanted_hash(
        &cached_path_subquery_for_store_hash_with_input_closure(
            "wanted.hash",
            "reader_build",
            "$3::text[]",
        ),
        &format!(
            "JOIN derivation_build reader_build ON reader_build.id = $2::uuid AND {}",
            derivation_build_has_build_job_not_aborted_and_evaluation_not_finished_sql_predicate(
                "reader_build"
            ),
        ),
    ),
        params = [CachedPathHashes(64), SharedBuildId, CachedPathHashes(64)];

    STORED_COPY_PER_WANTED_STORE_HASH_READABLE_BY_EXEC_JOB_QUERY = || per_wanted_hash(
        &stored_copies_for_store_hash_usable_by_exec_job_subquery(
            "wanted.hash",
            "$2::uuid",
            "$3::text[]",
        ),
        "",
    ),
        params = [CachedPathHashes(64), ProjectId, CachedPathHashes(64)];

    DERIVATION_ID_AND_BEST_RANKED_STORED_COPY_FROM_ANY_CACHE_FOR_STORE_PATH_HASHES_QUERY = || {
        select_derivation_id_and_chosen_stored_copy_row_for_unnested_derivation_store_path_hash_pairs_using_per_hash_subquery_on_shared_build(
            &cached_path_subquery_for_store_hash("wanted.hash"),
        )
    },
        params = [DerivationIds(64), CachedPathHashes(64)];

    DERIVATION_ID_AND_TRUSTED_CONTENT_VARIANT_ROW_FOR_OWN_OUTPUT_STORE_PATH_HASHES_ON_SHARED_BUILD_QUERY = || {
        select_derivation_id_and_chosen_stored_copy_row_for_unnested_derivation_store_path_hash_pairs_using_per_hash_subquery_on_shared_build(
            &select_best_stored_copy_for_own_build_outputs_trusted_by_all_projects_with_live_jobs_excluding_direct_use(
                "wanted.hash",
                "reader_build",
            ),
        )
    },
        params = [DerivationIds(64), CachedPathHashes(64)],
        tier = Hot,
        budget = crate::sql::Budget::hot().buffers(4_000)
            .because(RAISED_QUERY_BUDGET_REASON_FOR_TRUSTED_OUTPUT_CHECK_AND_STORED_COPY_READER_BECAUSE_EVERY_LIVE_BUILD_JOB_TRIGGERS_TRUST_CHECK);

    STORE_PATH_CONTENT_VARIANTS_BY_STORE_HASHES_FOR_MAINTENANCE_QUERY = || {
        "SELECT * FROM cached_path WHERE hash = ANY($1::text[]) ORDER BY hash, created_at, id".to_owned()
    },
        params = [CachedPathHashes(64)];
}

async fn rows_for_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
    statement: impl Fn(Vec<String>) -> Statement,
) -> Result<Vec<MCachedPath>, DbErr> {
    crate::fetch_in_chunks(hashes, |chunk| {
        let statement = statement(chunk);
        async move { MCachedPath::find_by_statement(statement).all(db).await }
    })
    .await
}

fn by_hash(rows: Vec<MCachedPath>) -> HashMap<String, MCachedPath> {
    rows.into_iter()
        .map(|row| (row.hash.clone(), row))
        .collect()
}

pub async fn get_content_variants_served_by_binary_cache_for_store_path_hashes<
    C: ConnectionTrait,
>(
    db: &C,
    cache: CacheId,
    hashes: &[String],
) -> Result<HashMap<String, MCachedPath>, DbErr> {
    let reader = Value::Uuid(Some(cache.into_inner()));
    rows_for_hashes(db, hashes, |chunk| {
        CONTENT_VARIANT_ROWS_SERVED_BY_BINARY_CACHE_FOR_STORE_HASHES_QUERY
            .bind([chunk.into(), reader.clone()])
    })
    .await
    .map(by_hash)
}

pub async fn highest_ranked_trusted_content_variant_by_store_path_hash_for_project<
    C: ConnectionTrait,
>(
    db: &C,
    project: ProjectId,
    hashes: &[String],
) -> Result<HashMap<String, MCachedPath>, DbErr> {
    let reader = Value::Uuid(Some(project.into_inner()));
    rows_for_hashes(db, hashes, |chunk| {
        TRUSTED_CONTENT_VARIANT_PER_STORE_HASH_FOR_PROJECT_QUERY
            .bind([chunk.into(), reader.clone()])
    })
    .await
    .map(by_hash)
}

pub async fn fetch_stored_copy_usable_as_build_input_of_shared_build_for_each_store_path_hash<
    C: ConnectionTrait,
>(
    db: &C,
    shared_build: DerivationBuildId,
    input_closure_set: &StorePathInputClosureHashes,
    hashes: &[String],
) -> Result<HashMap<String, MCachedPath>, DbErr> {
    read_shared_build_stored_copies_for_each_store_path_hash_at_instant(
        db,
        shared_build,
        input_closure_set,
        hashes,
        Instant::now(),
    )
    .await
}

async fn read_shared_build_stored_copies_for_each_store_path_hash_at_instant<C: ConnectionTrait>(
    db: &C,
    shared_build: DerivationBuildId,
    input_closure_set: &StorePathInputClosureHashes,
    hashes: &[String],
    current_read_instant: Instant,
) -> Result<HashMap<String, MCachedPath>, DbErr> {
    let input_closure = input_closure_set
        .input_closure_store_path_hashes(db, shared_build)
        .await?;
    let mut chosen = read_chosen_store_path_content_variant_rows_for_hashes_in_chunks(
        db,
        shared_build,
        &input_closure,
        hashes,
    )
    .await?;
    let requested_hashes_missing_stored_copy_outside_input_closure: Vec<&String> = hashes
        .iter()
        .filter(|requested_store_path_hash| {
            !chosen.contains_key(*requested_store_path_hash)
                && !input_closure.contains(*requested_store_path_hash)
        })
        .collect();
    if requested_hashes_missing_stored_copy_outside_input_closure.is_empty() {
        return Ok(chosen);
    }
    let input_closure = input_closure_set
        .refresh_input_closure_store_path_hashes_if_stale(db, shared_build, current_read_instant)
        .await?;
    let missed_hashes_to_read_again: Vec<String> =
        requested_hashes_missing_stored_copy_outside_input_closure
            .into_iter()
            .filter(|requested_store_path_hash| input_closure.contains(*requested_store_path_hash))
            .cloned()
            .collect();
    if !missed_hashes_to_read_again.is_empty() {
        chosen.extend(
            read_chosen_store_path_content_variant_rows_for_hashes_in_chunks(
                db,
                shared_build,
                &input_closure,
                &missed_hashes_to_read_again,
            )
            .await?,
        );
    }
    Ok(chosen)
}

async fn read_chosen_store_path_content_variant_rows_for_hashes_in_chunks<C: ConnectionTrait>(
    db: &C,
    shared_build: DerivationBuildId,
    input_closure: &HashSet<String>,
    hashes: &[String],
) -> Result<HashMap<String, MCachedPath>, DbErr> {
    let reader = Value::Uuid(Some(shared_build.into_inner()));
    rows_for_hashes(db, hashes, |chunk| {
        let in_input_closure: Vec<String> = chunk
            .iter()
            .filter(|requested_store_path_hash| input_closure.contains(*requested_store_path_hash))
            .cloned()
            .collect();
        STORED_COPY_BY_STORE_PATH_HASH_USABLE_AS_SHARED_BUILD_BUILD_INPUT_QUERY.bind([
            chunk.into(),
            reader.clone(),
            in_input_closure.into(),
        ])
    })
    .await
    .map(by_hash)
}

pub async fn get_stored_copy_readable_by_exec_job_for_each_store_path_hash<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    program: &str,
    program_closure_set: &StorePathInputClosureHashes,
    hashes: &[String],
) -> Result<HashMap<String, MCachedPath>, DbErr> {
    let program_closure = program_closure_set
        .load_exec_job_program_closure_store_path_hashes(db, program)
        .await?;
    let reader = Value::Uuid(Some(project.into_inner()));
    rows_for_hashes(db, hashes, |chunk| {
        let in_program_closure: Vec<String> = chunk
            .iter()
            .filter(|requested_store_path_hash| {
                program_closure.contains(*requested_store_path_hash)
            })
            .cloned()
            .collect();
        STORED_COPY_PER_WANTED_STORE_HASH_READABLE_BY_EXEC_JOB_QUERY.bind([
            chunk.into(),
            reader.clone(),
            in_program_closure.into(),
        ])
    })
    .await
    .map(by_hash)
}

pub async fn find_best_ranked_stored_copy_in_any_cache_by_derivation_id_and_store_path_hash<
    C: ConnectionTrait,
>(
    db: &C,
    wanted: &[(DerivationId, String)],
) -> Result<HashMap<(DerivationId, String), MCachedPath>, DbErr> {
    read_stored_copy_rows_by_derivation_store_path_hash_pairs(db, wanted, |values| {
        DERIVATION_ID_AND_BEST_RANKED_STORED_COPY_FROM_ANY_CACHE_FOR_STORE_PATH_HASHES_QUERY
            .bind(values)
    })
    .await
}

pub async fn highest_ranked_trusted_content_variant_by_derivation_id_and_store_path_hash_for_shared_build_own_outputs<
    C: ConnectionTrait,
>(
    db: &C,
    wanted: &[(DerivationId, String)],
) -> Result<HashMap<(DerivationId, String), MCachedPath>, DbErr> {
    read_stored_copy_rows_by_derivation_store_path_hash_pairs(db, wanted, |values| {
        DERIVATION_ID_AND_TRUSTED_CONTENT_VARIANT_ROW_FOR_OWN_OUTPUT_STORE_PATH_HASHES_ON_SHARED_BUILD_QUERY
            .bind(values)
    })
    .await
}

async fn read_stored_copy_rows_by_derivation_store_path_hash_pairs<C: ConnectionTrait>(
    db: &C,
    wanted: &[(DerivationId, String)],
    statement: impl Fn([Value; 2]) -> Statement,
) -> Result<HashMap<(DerivationId, String), MCachedPath>, DbErr> {
    let mut chosen = HashMap::new();
    for chunk in wanted.chunks(crate::IN_CHUNK_SIZE) {
        let (derivations, hashes): (Vec<uuid::Uuid>, Vec<String>) = chunk
            .iter()
            .map(|(derivation, hash)| (derivation.into_inner(), hash.clone()))
            .unzip();
        let rows = db
            .query_all_raw(statement([derivations.into(), hashes.into()]))
            .await?;
        for row in rows {
            let derivation = DerivationId::new(row.try_get::<uuid::Uuid>("", "reader_derivation")?);
            let variant = MCachedPath::from_query_result(&row, "")?;
            chosen.insert((derivation, variant.hash.clone()), variant);
        }
    }
    Ok(chosen)
}

pub async fn get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache<
    C: ConnectionTrait,
>(
    db: &C,
    reader: StorePathContentVariantReader,
    hashes: &[String],
) -> Result<HashMap<String, MCachedPath>, DbErr> {
    match reader {
        StorePathContentVariantReader::Project(project) => {
            highest_ranked_trusted_content_variant_by_store_path_hash_for_project(
                db, project, hashes,
            )
            .await
        }
        StorePathContentVariantReader::SharedBuild(shared_build, input_closure_set) => {
            fetch_stored_copy_usable_as_build_input_of_shared_build_for_each_store_path_hash(
                db,
                shared_build,
                &input_closure_set,
                hashes,
            )
            .await
        }
        StorePathContentVariantReader::ExecJob(project, program, program_closure_set) => {
            get_stored_copy_readable_by_exec_job_for_each_store_path_hash(
                db,
                project,
                &program,
                &program_closure_set,
                hashes,
            )
            .await
        }
        StorePathContentVariantReader::Cache(cache) => {
            get_content_variants_served_by_binary_cache_for_store_path_hashes(db, cache, hashes)
                .await
        }
    }
}

pub async fn trusted_content_variants_by_store_path_hash_for_project_best_ranked_first_up_to_count<
    C: ConnectionTrait,
>(
    db: &C,
    project: ProjectId,
    hash: &str,
    count: i64,
) -> Result<Vec<MCachedPath>, DbErr> {
    MCachedPath::find_by_statement(
        TRUSTED_CONTENT_VARIANTS_FOR_PROJECT_STORE_PATH_HASH_IN_RANK_ORDER_LIMIT_N_QUERY.bind([
            hash.into(),
            Value::Uuid(Some(project.into_inner())),
            count.into(),
        ]),
    )
    .all(db)
    .await
}

pub async fn list_all_content_variant_rows_for_store_path_hashes<C: ConnectionTrait>(
    db: &C,
    hashes: &[String],
) -> Result<Vec<MCachedPath>, DbErr> {
    rows_for_hashes(db, hashes, |chunk| {
        STORE_PATH_CONTENT_VARIANTS_BY_STORE_HASHES_FOR_MAINTENANCE_QUERY.bind([chunk.into()])
    })
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::predicates::content_variant_trusted_by_all_projects_with_live_job_on_shared_build;
    use gradient_fixtures::store_path_content_variants::{
        FIRST_KEY, SECOND_KEY, SHARED, UNTRUSTED, two_variants_of_one_hash_and_one_untrusted_hash,
    };

    fn key_of(rows: &HashMap<String, MCachedPath>, hash: &str) -> Option<String> {
        rows.get(hash).map(|row| row.nar_object_storage_key.clone())
    }

    #[test]
    fn worker_read_sql_has_no_recursive_keyword_and_trust_source_files_register_exactly_four_recursive_statements()
     {
        let mut sorted_recursive_registered_sql_statement_names_in_both_trust_source_files: Vec<
            &str,
        > = crate::sql::registry()
            .filter(|query| {
                query.file.ends_with("store_path_content_variant_trust.rs")
                    || query
                        .file
                        .ends_with("store_path_content_variant_reader_choice.rs")
            })
            .filter(|query| query.text().contains("RECURSIVE"))
            .map(|query| query.name)
            .collect();
        sorted_recursive_registered_sql_statement_names_in_both_trust_source_files.sort_unstable();
        assert_eq!(
            (
                STORED_COPY_BY_STORE_PATH_HASH_USABLE_AS_SHARED_BUILD_BUILD_INPUT_QUERY
                    .text()
                    .contains("RECURSIVE"),
                sorted_recursive_registered_sql_statement_names_in_both_trust_source_files,
            ),
            (
                false,
                vec![
                    "INSERT_PLACEHOLDERS_FOR_PROJECT_OUTPUTS_INTO_CACHE",
                    "INSERT_PLACEHOLDERS_FOR_PROJECT_STORE_HASHES",
                    "SHARED_BUILD_INPUT_CLOSURE_STORE_PATH_HASHES_QUERY",
                    "SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING",
                ]
            ),
            "A query for the input closure can traverse the build closure once per job. The worker read does not traverse the build closure. Exactly four statements in both trust files are recursive. They are the query for the input closure, the signature query for dependency outputs and two placeholder inserts."
        );
    }

    #[tokio::test]
    async fn test_selections_return_only_variant_trusted_by_reader() {
        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let conn = readers.db.connection();
        let hashes = [SHARED.to_owned()];
        let first = Some(FIRST_KEY.to_owned());
        let second = Some(SECOND_KEY.to_owned());

        let mut served = Vec::new();
        for cache in [readers.cache_one, readers.cache_two] {
            let rows = get_content_variants_served_by_binary_cache_for_store_path_hashes(
                &conn, cache, &hashes,
            )
            .await
            .expect("The selection ran.");
            served.push(key_of(&rows, SHARED));
        }
        assert_eq!(
            served,
            vec![first.clone(), second.clone()],
            "Cache C1 can provide only the variants that were built for project A. Cache C2 can provide only the variants that were uploaded to C2."
        );

        let mut consumed = Vec::new();
        for project in [readers.project_a, readers.project_b] {
            let rows = highest_ranked_trusted_content_variant_by_store_path_hash_for_project(
                &conn, project, &hashes,
            )
            .await
            .expect("The selection ran.");
            consumed.push(key_of(&rows, SHARED));
        }
        assert_eq!(
            consumed,
            vec![first.clone(), second.clone()],
            "Projects consume only the variants that they trust."
        );

        let mut built = Vec::new();
        for shared_build in [readers.build_a, readers.build_b] {
            let rows =
                fetch_stored_copy_usable_as_build_input_of_shared_build_for_each_store_path_hash(
                    &conn,
                    shared_build,
                    &StorePathInputClosureHashes::default(),
                    &hashes,
                )
                .await
                .expect("The selection ran.");
            built.push(key_of(&rows, SHARED));
        }
        assert_eq!(
            built,
            vec![first.clone(), second.clone()],
            "Shared builds consume only the variants that all projects with running jobs on that build trust."
        );

        let mut all: Vec<String> =
            list_all_content_variant_rows_for_store_path_hashes(&conn, &hashes)
                .await
                .expect("The selection ran.")
                .into_iter()
                .map(|row| row.nar_object_storage_key)
                .collect();
        all.sort();
        assert_eq!(
            all,
            vec![FIRST_KEY.to_owned(), SECOND_KEY.to_owned()],
            "Maintenance can recognize both variants of a hash."
        );
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn store_path_hash_only_untrusted_variant_signed_in_cache_c1_is_served_by_c1_and_project_a_selects_it_while_project_b_does_not()
     {
        let readers = two_variants_of_one_hash_and_one_untrusted_hash().await;
        let conn = readers.db.connection();
        let hashes = [UNTRUSTED.to_owned()];

        let mut served = Vec::new();
        for cache in [readers.cache_one, readers.cache_two] {
            served.push(
                get_content_variants_served_by_binary_cache_for_store_path_hashes(
                    &conn, cache, &hashes,
                )
                .await
                .expect("The selection ran.")
                .contains_key(UNTRUSTED),
            );
        }
        assert_eq!(
            served,
            vec![true, false],
            "The signature for that variant is C1's own, so C1 can provide it. No such signature is in C2, so C2 can provide nothing."
        );

        let mut selected = Vec::new();
        for project in [readers.project_a, readers.project_b] {
            selected.push(
                highest_ranked_trusted_content_variant_by_store_path_hash_for_project(
                    &conn, project, &hashes,
                )
                .await
                .expect("The selection ran.")
                .contains_key(UNTRUSTED),
            );
        }
        for shared_build in [readers.build_a, readers.build_b] {
            selected.push(
                fetch_stored_copy_usable_as_build_input_of_shared_build_for_each_store_path_hash(
                    &conn,
                    shared_build,
                    &StorePathInputClosureHashes::default(),
                    &hashes,
                )
                .await
                .expect("The selection ran.")
                .contains_key(UNTRUSTED),
            );
        }
        assert_eq!(
            selected,
            vec![true, false, true, false],
            "Project A read cache C1. C1 had a signed row for that variant. Project A and its shared builds use that variant. Project B and its shared builds don't use that variant."
        );

        let all: Vec<String> = list_all_content_variant_rows_for_store_path_hashes(&conn, &hashes)
            .await
            .expect("The selection ran.")
            .into_iter()
            .map(|row| row.hash)
            .collect();
        assert_eq!(
            all,
            vec![UNTRUSTED.to_owned()],
            "Maintenance can list the variant."
        );
        readers.db.finish().await;
    }

    #[tokio::test]
    async fn worker_and_reading_project_select_copy_when_read_cache_has_unsigned_placeholder_but_write_only_subscription_nonreading_project_and_inactive_cache_do_not()
     {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_fixtures::shape::shape;
        use sea_orm::ActiveEnum;

        let output_store_path_hash = "4".repeat(32);
        let worker_project_evaluation_id = EvaluationId::now_v7();
        let subscriber_reading_signed_output_evaluation_id = EvaluationId::now_v7();
        let evaluation_id_of_project_that_neither_built_output_nor_reads_cache =
            EvaluationId::now_v7();
        let signed_output_row_cache_id = CacheId::now_v7();
        let db = shape()
            .evaluation(worker_project_evaluation_id)
            .cached_path(&output_store_path_hash, "built")
            .mark_cached_path_built_by_team_worker()
            .into_cache(signed_output_row_cache_id)
            .evaluation(subscriber_reading_signed_output_evaluation_id)
            .into_cache(signed_output_row_cache_id)
            .evaluation(evaluation_id_of_project_that_neither_built_output_nor_reads_cache)
            .into_database()
            .await;
        let projects = [
            db.project_of(worker_project_evaluation_id).await,
            db.project_of(subscriber_reading_signed_output_evaluation_id)
                .await,
            db.project_of(evaluation_id_of_project_that_neither_built_output_nor_reads_cache)
                .await,
        ];
        let reading_project = projects[1];
        db.execute(&format!(
            "UPDATE project_cache SET mode = {read_only}, sign_outputs = false WHERE project = '{reading_project}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             SELECT uuidv7(), id, '{signed_output_row_cache_id}', 0, now() \
             FROM cached_path WHERE hash = '{output_store_path_hash}';",
            read_only = CacheSubscriptionMode::ReadOnly.to_value(),
        ))
        .await;
        let conn = db.connection();
        let reader_choice_selects_test_output_for_project = async |project| {
            highest_ranked_trusted_content_variant_by_store_path_hash_for_project(
                &conn,
                project,
                std::slice::from_ref(&output_store_path_hash),
            )
            .await
            .expect("The selection ran.")
            .contains_key(&output_store_path_hash)
        };

        let mut selected = Vec::new();
        for project in projects {
            selected.push(reader_choice_selects_test_output_for_project(project).await);
        }
        assert_eq!(
            selected,
            vec![true, true, false],
            "The worker project and projects that read a cache with unsigned placeholder rows select that copy. Projects that don't read that cache don't select that copy."
        );

        let trust_condition_removal_and_restore_sql_statement_pairs = [
            (
                format!(
                    "UPDATE project_cache SET mode = {}, sign_outputs = false WHERE project = '{reading_project}'",
                    CacheSubscriptionMode::WriteOnly.to_value()
                ),
                format!(
                    "UPDATE project_cache SET mode = {}, sign_outputs = false WHERE project = '{reading_project}'",
                    CacheSubscriptionMode::ReadOnly.to_value()
                ),
            ),
            (
                format!(
                    "UPDATE cache SET active = false WHERE id = '{signed_output_row_cache_id}'"
                ),
                format!("UPDATE cache SET active = true WHERE id = '{signed_output_row_cache_id}'"),
            ),
        ];
        let mut selected_after_removal = Vec::new();
        for (
            trust_condition_removal_statement,
            sql_statement_that_restores_removed_trust_condition,
        ) in trust_condition_removal_and_restore_sql_statement_pairs
        {
            db.execute(&trust_condition_removal_statement).await;
            selected_after_removal
                .push(reader_choice_selects_test_output_for_project(reading_project).await);
            if !sql_statement_that_restores_removed_trust_condition.is_empty() {
                db.execute(&sql_statement_that_restores_removed_trust_condition)
                    .await;
            }
        }
        assert_eq!(
            selected_after_removal,
            vec![false, false],
            "Reading projects stop selecting copies when the subscription is WriteOnly or the cache is inactive."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn cache_keeps_serving_worker_signed_store_path_after_signing_project_disables_signing_changes_access_mode_or_unsubscribes_or_other_project_signs_without_trust_and_after_worker_deletion_until_cache_deactivated_or_signature_null()
     {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use gradient_fixtures::shape::shape;
        use sea_orm::ActiveEnum;

        let worker_built_store_path_hash_signed_by_cache_before_signing_disabled = "5".repeat(32);
        let store_path_hash_without_signature = "6".repeat(32);
        let package_builder_project_evaluation_id = EvaluationId::now_v7();
        let second_project_evaluation_with_signing_and_no_trust_in_first_worker =
            EvaluationId::now_v7();
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(package_builder_project_evaluation_id)
            .cached_path(
                &worker_built_store_path_hash_signed_by_cache_before_signing_disabled,
                "built",
            )
            .cached_path(&store_path_hash_without_signature, "later")
            .into_cache(cache)
            .keyed("signing-key")
            .evaluation(second_project_evaluation_with_signing_and_no_trust_in_first_worker)
            .into_cache(cache)
            .into_database()
            .await;
        let package_builder_project = db.project_of(package_builder_project_evaluation_id).await;
        let second_project = db
            .project_of(second_project_evaluation_with_signing_and_no_trust_in_first_worker)
            .await;
        let read_only = CacheSubscriptionMode::ReadOnly.to_value();
        let write_only = CacheSubscriptionMode::WriteOnly.to_value();
        db.execute(&format!(
            "UPDATE project_cache SET mode = {read_only}, sign_outputs = false WHERE project = '{second_project}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), id, '{cache}', convert_to('signature', 'UTF8'), 0, now() \
             FROM cached_path WHERE hash = '{worker_built_store_path_hash_signed_by_cache_before_signing_disabled}';"
        ))
        .await;
        let conn = db.connection();
        let hashes = [
            worker_built_store_path_hash_signed_by_cache_before_signing_disabled.clone(),
            store_path_hash_without_signature.clone(),
        ];
        let store_path_hashes_served_to_outside_readers = async || {
            get_content_variants_served_by_binary_cache_for_store_path_hashes(&conn, cache, &hashes)
                .await
                .expect("The selection ran.")
        };

        let sql_statements_that_stop_cache_from_signing_new_content = [
            String::new(),
            format!(
                "UPDATE project_cache SET sign_outputs = false WHERE project = '{package_builder_project}'; \
                 INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
                 SELECT uuidv7(), id, '{cache}', NULL, 0, now() \
                 FROM cached_path WHERE hash = '{store_path_hash_without_signature}';"
            ),
            format!(
                "UPDATE project_cache SET mode = {read_only}, sign_outputs = false WHERE project = '{package_builder_project}'"
            ),
            format!(
                "UPDATE project_cache SET mode = {write_only}, sign_outputs = false WHERE project = '{package_builder_project}'"
            ),
            format!("DELETE FROM project_cache WHERE project = '{package_builder_project}'"),
            format!(
                "UPDATE project_cache SET mode = {write_only}, sign_outputs = true WHERE project = '{second_project}'"
            ),
            "DELETE FROM worker_registration".to_owned(),
        ];
        let mut signed_store_path_served = Vec::new();
        for statement in sql_statements_that_stop_cache_from_signing_new_content {
            if !statement.is_empty() {
                db.execute(&statement).await;
            }
            signed_store_path_served.push(
                store_path_hashes_served_to_outside_readers()
                    .await
                    .contains_key(
                        &worker_built_store_path_hash_signed_by_cache_before_signing_disabled,
                    ),
            );
        }
        assert_eq!(
            signed_store_path_served,
            vec![true; 7],
            "The cache can serve all paths that it signed with its own key. These paths remain available after signing is off. The same is true after a subscription change or end, after another project signed without trusting the worker and after deletion of the worker."
        );
        assert!(
            !store_path_hashes_served_to_outside_readers()
                .await
                .contains_key(&store_path_hash_without_signature),
            "Cache lookups return no path with only placeholder rows that have no signature (for example, a path saved after signature invalidation)."
        );

        db.execute(&format!(
            "UPDATE cache SET active = false WHERE id = '{cache}'"
        ))
        .await;
        let served_by_inactive_cache = store_path_hashes_served_to_outside_readers().await;
        db.execute(&format!(
            "UPDATE cache SET active = true WHERE id = '{cache}'; \
             UPDATE cached_path_signature SET signature = NULL, previous_signing_key_signature = NULL"
        ))
        .await;
        assert!(
            served_by_inactive_cache.is_empty()
                && store_path_hashes_served_to_outside_readers()
                    .await
                    .is_empty(),
            "Inactive caches provide nothing. A null value in the signature line can remove service from a path."
        );
        db.finish().await;
    }

    use crate::store_path_content_variant_trust::tests::{
        DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH, DEPENDENCY_OUTPUT_STORE_PATH_HASH,
        StorePathContentVariantTrustTestFixture,
        setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache,
    };
    use gradient_fixtures::shape::Shape;

    #[derive(Debug, Default, PartialEq, Eq)]
    struct ParentBuildStorePathThreeReaderAnswers {
        parent_build_has_zero_blocking_deps_after_recount: bool,
        package_name: Option<String>,
        returned_store_path_copy_package_name: Option<String>,
    }

    impl ParentBuildStorePathThreeReaderAnswers {
        fn all_three_readers_return_stored_copy_for_package(package: &str) -> Self {
            Self {
                parent_build_has_zero_blocking_deps_after_recount: true,
                package_name: Some(package.to_owned()),
                returned_store_path_copy_package_name: Some(package.to_owned()),
            }
        }
    }

    async fn build_stored_dependency_fixture_and_apply_one_sql_change(
        waiting_project_has_read_write_cache_with_private_signing_key: bool,
        add_rows_to_test_shape_builder: impl FnOnce(Shape, &str) -> Shape,
        sql_run_on_fixture_before_query: &str,
    ) -> StorePathContentVariantTrustTestFixture {
        let fixture =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                waiting_project_has_read_write_cache_with_private_signing_key,
                add_rows_to_test_shape_builder,
            )
            .await;
        if !sql_run_on_fixture_before_query.is_empty() {
            fixture.db.execute(sql_run_on_fixture_before_query).await;
        }
        fixture
    }

    async fn read_store_path_content_variant_reader_choice_for_parent_build(
        fixture: &StorePathContentVariantTrustTestFixture,
        input_closure_set: &StorePathInputClosureHashes,
        hash: &str,
    ) -> ParentBuildStorePathThreeReaderAnswers {
        let parent = fixture.parent;
        let conn = fixture.db.connection();
        crate::graph::can_start::recount_blocking_deps_for_derivation_ids(&conn, &[parent])
            .await
            .expect("The recount ran.");
        let parent_build_has_zero_blocking_deps_after_recount = fixture
            .db
            .texts(&format!(
                "SELECT (blocking_deps = 0)::text FROM derivation_build WHERE derivation = '{parent}'"
            ))
            .await
            == ["true"];
        let shared_build = DerivationBuildId::new(
            fixture
                .db
                .texts(&format!(
                    "SELECT id::text FROM derivation_build WHERE derivation = '{parent}'"
                ))
                .await[0]
                .parse()
                .expect("a uuid"),
        );
        let derivation_id_and_requested_store_path_hash = (parent, hash.to_owned());
        ParentBuildStorePathThreeReaderAnswers {
            parent_build_has_zero_blocking_deps_after_recount,
            package_name:
                fetch_stored_copy_usable_as_build_input_of_shared_build_for_each_store_path_hash(
                    &conn,
                    shared_build,
                    input_closure_set,
                    std::slice::from_ref(&derivation_id_and_requested_store_path_hash.1),
                )
                .await
                .expect("The selection ran.")
                .remove(hash)
                .map(|row| row.package),
            returned_store_path_copy_package_name:
                find_best_ranked_stored_copy_in_any_cache_by_derivation_id_and_store_path_hash(
                    &conn,
                    std::slice::from_ref(&derivation_id_and_requested_store_path_hash),
                )
                .await
                .expect("The selection ran.")
                .remove(&derivation_id_and_requested_store_path_hash)
                .map(|row| row.package),
        }
    }

    fn sql_insert_second_dependency_output_store_path_copy_with_nar_hash_and_optional_signature_in_cache(
        nar_hash: &str,
        optional_signature_row_cache_id_and_signature_value_sql_expressions: Option<(&str, &str)>,
    ) -> String {
        let second_stored_dependency_output_copy_optional_signature_insert_sql_or_empty = optional_signature_row_cache_id_and_signature_value_sql_expressions.map_or(String::new(), |(cache, signature)| {
            format!(
                "INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
                 SELECT uuidv7(), id, {cache}, {signature}, 0, now() FROM cached_path \
                 WHERE nar_object_storage_key = 'other-variant';"
            )
        });
        format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES (uuidv7(), '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}', 'other-variant', 'other-variant', 'sha256:other', {nar_hash}, 5, now()); {second_stored_dependency_output_copy_optional_signature_insert_sql_or_empty}"
        )
    }

    fn sql_that_marks_derivation_as_fixed_output(hash: &str) -> String {
        format!(
            "UPDATE derivation SET is_fixed_output = true WHERE id IN \
             (SELECT derivation FROM derivation_output WHERE hash = '{hash}')"
        )
    }

    async fn build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(
        waiting_project_has_read_write_cache_with_private_signing_key: bool,
        add_rows_to_test_shape_builder: impl FnOnce(Shape, &str) -> Shape,
        sql_run_on_fixture_before_query: &str,
    ) -> ParentBuildStorePathThreeReaderAnswers {
        let fixture = build_stored_dependency_fixture_and_apply_one_sql_change(
            waiting_project_has_read_write_cache_with_private_signing_key,
            add_rows_to_test_shape_builder,
            sql_run_on_fixture_before_query,
        )
        .await;
        let answers = read_store_path_content_variant_reader_choice_for_parent_build(
            &fixture,
            &StorePathInputClosureHashes::default(),
            DEPENDENCY_OUTPUT_STORE_PATH_HASH,
        )
        .await;
        fixture.db.finish().await;
        answers
    }

    #[tokio::test]
    async fn test_project_without_keyed_cache_trusts_stored_input_and_returns_it_from_both_build_readers()
     {
        assert_eq!(
            build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(false, |shape, _| shape, "").await,
            ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
            "A project without a keyed ReadWrite cache can use the stored copy of the input directly. The edge check and both build readers return that copy."
        );
    }

    #[tokio::test]
    async fn stored_worker_built_copy_is_used_directly_even_when_keyed_read_write_cache_is_available()
     {
        assert_eq!(
            build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(true, |shape, _| shape, "").await,
            ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
            "A worker can use the copy that the worker built directly, even with a keyed ReadWrite cache on the project that can receive signed copies. The build does not wait for a signature."
        );
    }

    #[tokio::test]
    async fn project_without_cache_subscription_uses_other_projects_stored_copy_as_build_input_and_parent_build_is_not_blocked()
     {
        let private_binary_cache_id_not_read_by_waiting_project = CacheId::now_v7();
        let dependency = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .into_cache(private_binary_cache_id_not_read_by_waiting_project)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
            .mark_uploaded_by_user()
            .evaluation(EvaluationId::now_v7())
            .derivation(&parent)
            .depends_on(&dependency)
            .job()
            .into_database()
            .await;
        let fixture = StorePathContentVariantTrustTestFixture {
            parent: db.id_of(&parent).await,
            cache: private_binary_cache_id_not_read_by_waiting_project,
            db,
        };
        let answers = read_store_path_content_variant_reader_choice_for_parent_build(
            &fixture,
            &StorePathInputClosureHashes::default(),
            DEPENDENCY_OUTPUT_STORE_PATH_HASH,
        )
        .await;
        crate::graph::can_start::recount_blocking_deps_for_derivation_ids(
            &fixture.db.connection(),
            &[fixture.parent],
        )
        .await
        .expect("The recount ran.");
        let parent_build_blocking_dependency_count_after_recount =
            fixture.db.shared_build(&parent).await.blocking_deps;
        fixture.db.finish().await;
        assert_eq!(
            (answers, parent_build_blocking_dependency_count_after_recount),
            (
                ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package(
                    "dependency"
                ),
                0
            ),
            "A project with no subscription to the private cache of another project can still use a stored copy in that cache as a build input. Both build readers return that copy. The build of that project does not wait for any dependency."
        );
    }

    async fn run_publish_pass_for_parent_build_and_return_signed_and_served_store_path_hashes(
        fixture: &StorePathContentVariantTrustTestFixture,
    ) -> (Vec<String>, Vec<String>) {
        let conn = fixture.db.connection();
        let rows_to_sign = crate::store_path_content_variant_trust::load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
            &conn,
            &[fixture.parent],
        )
        .await
        .expect("The publish query ran.");
        for row in &rows_to_sign {
            fixture
                .db
                .execute(&format!(
                    "INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at, reference_closure_established_at_cache_deletion_count) \
                     VALUES (uuidv7(), '{}', '{}', 'x'::bytea, 0, now(), 0) ON CONFLICT DO NOTHING",
                    row.cached_path, row.cache
                ))
                .await;
        }
        crate::graph::can_start::recount_blocking_deps_for_derivation_ids(&conn, &[fixture.parent])
            .await
            .expect("The recount ran.");
        let cache = fixture.cache;
        let signed = fixture
            .db
            .texts(&format!(
                "SELECT stored_copy.hash FROM cached_path_signature signature \
                 JOIN cached_path stored_copy ON stored_copy.id = signature.cached_path \
                 WHERE signature.cache = '{cache}' AND signature.signature IS NOT NULL \
                 ORDER BY stored_copy.hash"
            ))
            .await;
        let mut served: Vec<String> =
            get_trusted_content_variant_for_each_store_path_hash_for_project_shared_build_or_binary_cache(
                &conn,
                StorePathContentVariantReader::Cache(cache),
                &[
                    DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(),
                    DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned(),
                ],
            )
            .await
            .expect("The cache reader ran.")
            .into_keys()
            .collect();
        served.sort();
        (signed, served)
    }

    #[tokio::test]
    async fn dependency_output_only_uploaded_to_other_project_private_cache_is_used_as_build_input_but_not_signed_or_served_from_waiting_project_cache()
     {
        let private_cache_of_other_project = CacheId::now_v7();
        let signing_cache_of_waiting_project = CacheId::now_v7();
        let dependency = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .into_cache(private_cache_of_other_project)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
            .mark_uploaded_by_user()
            .evaluation(EvaluationId::now_v7())
            .derivation(&parent)
            .depends_on(&dependency)
            .job()
            .into_cache(signing_cache_of_waiting_project)
            .keyed("signing-key")
            .into_database()
            .await;
        let fixture = StorePathContentVariantTrustTestFixture {
            parent: db.id_of(&parent).await,
            cache: signing_cache_of_waiting_project,
            db,
        };
        let signed_and_served =
            run_publish_pass_for_parent_build_and_return_signed_and_served_store_path_hashes(
                &fixture,
            )
            .await;
        let answers = read_store_path_content_variant_reader_choice_for_parent_build(
            &fixture,
            &StorePathInputClosureHashes::default(),
            DEPENDENCY_OUTPUT_STORE_PATH_HASH,
        )
        .await;
        let parent_build_blocking_dependency_count_after_publish_pass =
            fixture.db.shared_build(&parent).await.blocking_deps;
        fixture.db.finish().await;
        assert_eq!(
            (
                signed_and_served,
                answers,
                parent_build_blocking_dependency_count_after_publish_pass
            ),
            (
                (Vec::new(), Vec::new()),
                ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package(
                    "dependency"
                ),
                0
            ),
            "Dependency outputs stay unsigned in the publish pass when they exist only as uploads in another project's private cache. A waiting project can't read that cache. No signature and no copy come from the waiting project's signing cache. Parent builds still use that upload as a build input, with no dependency wait."
        );
    }

    #[tokio::test]
    async fn publish_signs_nothing_from_parent_build_when_worker_built_dependency_output_reference_exists_only_as_upload_in_private_cache_not_read_by_waiting_project()
     {
        let private_cache_of_other_project = CacheId::now_v7();
        let signing_cache_of_waiting_project = CacheId::now_v7();
        let dependency = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
            .references(&[(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH, "reference")])
            .into_cache(private_cache_of_other_project)
            .cached_path(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH, "reference")
            .mark_uploaded_by_user()
            .evaluation(EvaluationId::now_v7())
            .derivation(&parent)
            .depends_on(&dependency)
            .job()
            .into_cache(signing_cache_of_waiting_project)
            .keyed("signing-key")
            .into_database()
            .await;
        let fixture = StorePathContentVariantTrustTestFixture {
            parent: db.id_of(&parent).await,
            cache: signing_cache_of_waiting_project,
            db,
        };
        let signed_and_served =
            run_publish_pass_for_parent_build_and_return_signed_and_served_store_path_hashes(
                &fixture,
            )
            .await;
        fixture.db.finish().await;
        assert_eq!(
            signed_and_served,
            (Vec::new(), Vec::new()),
            "The publish pass can't add a signature for the parent build in one case. The references of a dependency output from a worker exist only as uploads in a private cache. The waiting project can't read that cache. Neither the dependency output nor the references get a signature in the signing cache of the waiting project."
        );
    }

    #[tokio::test]
    async fn publish_signs_worker_built_dependency_output_copy_and_uploaded_reference_copy_read_into_waiting_project_signing_cache_and_served_from_it()
     {
        let worker_built_fixture =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape,
            )
            .await;
        let worker_built_signed_and_served =
            run_publish_pass_for_parent_build_and_return_signed_and_served_store_path_hashes(
                &worker_built_fixture,
            )
            .await;
        worker_built_fixture.db.finish().await;
        let cache_read_by_waiting_project = CacheId::now_v7();
        let signing_cache_of_waiting_project = CacheId::now_v7();
        let dependency = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let db = gradient_fixtures::shape::shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
            .references(&[(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH, "reference")])
            .evaluation(EvaluationId::now_v7())
            .derivation(&parent)
            .depends_on(&dependency)
            .job()
            .into_cache(cache_read_by_waiting_project)
            .cached_path(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH, "reference")
            .mark_uploaded_by_user()
            .into_cache(signing_cache_of_waiting_project)
            .keyed("signing-key")
            .into_database()
            .await;
        let uploaded_reference_fixture = StorePathContentVariantTrustTestFixture {
            parent: db.id_of(&parent).await,
            cache: signing_cache_of_waiting_project,
            db,
        };
        let uploaded_reference_signed_and_served =
            run_publish_pass_for_parent_build_and_return_signed_and_served_store_path_hashes(
                &uploaded_reference_fixture,
            )
            .await;
        uploaded_reference_fixture.db.finish().await;
        let dependency_output_and_reference = vec![
            DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(),
            DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned(),
        ];
        assert_eq!(
            (
                worker_built_signed_and_served,
                uploaded_reference_signed_and_served
            ),
            (
                (
                    dependency_output_and_reference.clone(),
                    dependency_output_and_reference.clone()
                ),
                (
                    dependency_output_and_reference.clone(),
                    dependency_output_and_reference
                )
            ),
            "The publish pass can sign and store two kinds of copies in the signing cache of the waiting project. The copies are outputs of dependencies that a worker built and references that were uploaded to the cache that the waiting project can read. The signing cache can then serve both."
        );
    }

    async fn assert_shared_build_worker_read_returns_no_stored_dependency_output_copy_after_running_sql_statement_on_stored_dependency_fixture(
        sql_run_on_fixture_before_query: &str,
    ) {
        assert_eq!(
            build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(
                false,
                |shape, _| shape,
                sql_run_on_fixture_before_query
            )
            .await
            .package_name,
            None,
            "The shared build reader can return nothing if there are no live jobs for the project on that build."
        );
    }

    #[tokio::test]
    async fn stored_copy_is_not_used_directly_when_project_job_on_parent_build_is_aborted() {
        assert_shared_build_worker_read_returns_no_stored_dependency_output_copy_after_running_sql_statement_on_stored_dependency_fixture(
            "UPDATE build_job SET aborted = true \
             WHERE derivation IN (SELECT derivation FROM derivation_dependency)",
        )
        .await;
    }

    #[tokio::test]
    async fn stored_copy_not_used_directly_after_project_job_evaluation_finishes() {
        assert_shared_build_worker_read_returns_no_stored_dependency_output_copy_after_running_sql_statement_on_stored_dependency_fixture(&format!(
            "UPDATE evaluation SET status = {terminal}, building_started_at = now() \
             WHERE id IN (SELECT evaluation FROM build_job \
                          WHERE derivation IN (SELECT derivation FROM derivation_dependency))",
            terminal = crate::sql::status::eval_in(
                &gradient_entity::evaluation::EvaluationStatus::TERMINAL[..1]
            ),
        ))
        .await;
    }

    #[tokio::test]
    async fn uploaded_store_path_copy_is_returned_by_all_three_store_path_readers_before_and_after_abort_of_active_parent_build_job_for_project_without_cache_access()
     {
        let remaining_project_reader_binary_cache_id = CacheId::now_v7();
        let fixture = build_stored_dependency_fixture_and_apply_one_sql_change(
            true,
            |shape, parent_build_derivation_path| {
                shape
                    .evaluation(EvaluationId::now_v7())
                    .derivation(parent_build_derivation_path)
                    .job()
                    .into_cache(remaining_project_reader_binary_cache_id)
                    .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
                    .mark_uploaded_by_user()
            },
            "",
        )
        .await;
        let readiness_worker_and_output_size_answers_while_job_live =
            read_store_path_content_variant_reader_choice_for_parent_build(
                &fixture,
                &StorePathInputClosureHashes::default(),
                DEPENDENCY_OUTPUT_STORE_PATH_HASH,
            )
            .await;
        fixture
            .db
            .execute(&format!(
                "UPDATE build_job SET aborted = true \
                 WHERE derivation IN (SELECT derivation FROM derivation_dependency) \
                   AND evaluation IN (SELECT keyed_evaluation.id FROM evaluation keyed_evaluation \
                                      JOIN task keyed_task ON keyed_task.id = keyed_evaluation.task \
                                      JOIN project_cache keyed_subscription \
                                        ON keyed_subscription.project = keyed_task.project \
                                      WHERE keyed_subscription.cache = '{}')",
                fixture.cache,
            ))
            .await;
        let three_reader_answers_after_keyed_signing_cache_job_abort =
            read_store_path_content_variant_reader_choice_for_parent_build(
                &fixture,
                &StorePathInputClosureHashes::default(),
                DEPENDENCY_OUTPUT_STORE_PATH_HASH,
            )
            .await;
        fixture.db.finish().await;
        assert_eq!(
            readiness_worker_and_output_size_answers_while_job_live,
            ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
            "Project K can't read the upload cache. A job of Project K on the parent build can still use the uploaded copy as a build input. All three readers return that copy while the job is active."
        );
        assert_eq!(
            three_reader_answers_after_keyed_signing_cache_job_abort,
            ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
            "The cancellation of the job of K left only project L with a running job on the build. L can read the cache with the upload, so all three readers return the uploaded copy."
        );
    }

    #[tokio::test]
    async fn store_path_outside_parent_input_closure_is_not_used_directly() {
        let fixture = build_stored_dependency_fixture_and_apply_one_sql_change(
            false,
            |shape, _| shape,
            &format!(
                "UPDATE cached_path SET \"references\" = NULL \
                 WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ),
        )
        .await;
        let answers = read_store_path_content_variant_reader_choice_for_parent_build(
            &fixture,
            &StorePathInputClosureHashes::default(),
            DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
        )
        .await;
        fixture.db.finish().await;
        assert_eq!(
            (
                answers.package_name,
                answers.returned_store_path_copy_package_name
            ),
            (None, Some("reference".to_owned())),
            "The third reader is the size lookup. It does not check membership in the input closure. The size lookup can return the stored copy of the reference. The edge check and the worker read don't use store paths outside the input closure."
        );
    }

    #[tokio::test]
    async fn read_reader_serves_same_job_set_after_reference_removal_and_refuses_fresh_job_set_after_walk()
     {
        let fixture =
            build_stored_dependency_fixture_and_apply_one_sql_change(false, |shape, _| shape, "")
                .await;
        let first_dispatched_job_input_closure_set = StorePathInputClosureHashes::default();
        let mut package_names_read_for_first_job_set = vec![
            read_store_path_content_variant_reader_choice_for_parent_build(
                &fixture,
                &first_dispatched_job_input_closure_set,
                DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
            )
            .await
            .package_name,
        ];
        fixture
            .db
            .execute(&format!(
                "UPDATE cached_path SET \"references\" = NULL \
                 WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ))
            .await;
        for _ in 0..3 {
            package_names_read_for_first_job_set.push(
                read_store_path_content_variant_reader_choice_for_parent_build(
                    &fixture,
                    &first_dispatched_job_input_closure_set,
                    DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
                )
                .await
                .package_name,
            );
        }
        let package_name_read_for_new_job_set =
            read_store_path_content_variant_reader_choice_for_parent_build(
                &fixture,
                &StorePathInputClosureHashes::default(),
                DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
            )
            .await
            .package_name;
        fixture.db.finish().await;
        assert_eq!(
            package_names_read_for_first_job_set,
            vec![Some("reference".to_owned()); 4],
            "Worker reads for a single job walk the input closure only once. Three more reads with the same job set still return a reference after removal of that reference from the closure in the database."
        );
        assert_eq!(
            package_name_read_for_new_job_set, None,
            "A new job can rescan to compute its own set and refuse references that no longer exist in the input closure."
        );
    }

    #[tokio::test]
    async fn store_path_added_to_inputs_after_first_read_is_served_after_one_recompute() {
        let fixture =
            build_stored_dependency_fixture_and_apply_one_sql_change(false, |shape, _| shape, "")
                .await;
        let original_stored_dependency_references_text = fixture
            .db
            .texts(&format!(
                "SELECT \"references\" FROM cached_path WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ))
            .await
            .remove(0);
        fixture
            .db
            .execute(&format!(
                "UPDATE cached_path SET \"references\" = NULL \
                 WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ))
            .await;
        let first_dispatched_job_input_closure_set = StorePathInputClosureHashes::default();
        let first_read_package_name_for_dependency_output_without_references =
            read_store_path_content_variant_reader_choice_for_parent_build(
                &fixture,
                &first_dispatched_job_input_closure_set,
                DEPENDENCY_OUTPUT_STORE_PATH_HASH,
            )
            .await
            .package_name;
        fixture
            .db
            .execute(&format!(
                "UPDATE cached_path SET \"references\" = '{original_stored_dependency_references_text}' \
                 WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ))
            .await;
        let package_name_from_later_worker_read_after_reference_store_path_put_back =
            read_store_path_content_variant_reader_choice_for_parent_build(
                &fixture,
                &first_dispatched_job_input_closure_set,
                DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
            )
            .await
            .package_name;
        fixture.db.finish().await;
        assert_eq!(
            (
                first_read_package_name_for_dependency_output_without_references,
                package_name_from_later_worker_read_after_reference_store_path_put_back
            ),
            (Some("dependency".to_owned()), Some("reference".to_owned())),
            "Entries in the input closure set come from the first worker read of the job. No references appear in the stored copy of the dependency. That reference can enter the input closure later. Reads of that reference in the same job set count as misses. Those reads recompute the set and return the stored copy of that reference."
        );
    }

    async fn read_parent_build_store_path_hash_package_name_from_worker(
        fixture: &StorePathContentVariantTrustTestFixture,
        input_closure_set: &StorePathInputClosureHashes,
        hash: &str,
        now: Instant,
    ) -> Option<String> {
        let parent = fixture.parent;
        let shared_build = DerivationBuildId::new(
            fixture
                .db
                .texts(&format!(
                    "SELECT id::text FROM derivation_build WHERE derivation = '{parent}'"
                ))
                .await[0]
                .parse()
                .expect("a uuid"),
        );
        read_shared_build_stored_copies_for_each_store_path_hash_at_instant(
            &fixture.db.connection(),
            shared_build,
            input_closure_set,
            &[hash.to_owned()],
            now,
        )
        .await
        .expect("The selection ran.")
        .remove(hash)
        .map(|row| row.package)
    }

    #[tokio::test]
    async fn test_miss_within_minimum_interval_skips_database_walk_and_recomputes_after_interval() {
        let fixture =
            build_stored_dependency_fixture_and_apply_one_sql_change(false, |shape, _| shape, "")
                .await;
        let original_stored_dependency_references_text = fixture
            .db
            .texts(&format!(
                "SELECT \"references\" FROM cached_path WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ))
            .await
            .remove(0);
        fixture
            .db
            .execute(&format!(
                "UPDATE cached_path SET \"references\" = NULL \
                 WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ))
            .await;
        let first_dispatched_job_input_closure_set = StorePathInputClosureHashes::default();
        let recompute_after_miss_instant = Instant::now();
        let mut
        package_names_read_by_worker_at_controlled_instants_around_minimum_recompute_interval =
            Vec::new();
        for (requested_store_path_hash, simulated_worker_read_instant) in [
            (
                DEPENDENCY_OUTPUT_STORE_PATH_HASH,
                recompute_after_miss_instant,
            ),
            (
                DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
                recompute_after_miss_instant,
            ),
        ] {
            package_names_read_by_worker_at_controlled_instants_around_minimum_recompute_interval
                .push(
                    read_parent_build_store_path_hash_package_name_from_worker(
                        &fixture,
                        &first_dispatched_job_input_closure_set,
                        requested_store_path_hash,
                        simulated_worker_read_instant,
                    )
                    .await,
                );
        }
        let store_saved_references_sql = format!(
            "UPDATE cached_path SET \"references\" = '{original_stored_dependency_references_text}' \
             WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
        );
        fixture.db.execute(&store_saved_references_sql).await;
        for simulated_worker_read_instant in [
            recompute_after_miss_instant + MIN_INPUT_CLOSURE_SET_RECOMPUTE_INTERVAL
                - Duration::from_secs(1),
            recompute_after_miss_instant + MIN_INPUT_CLOSURE_SET_RECOMPUTE_INTERVAL,
        ] {
            package_names_read_by_worker_at_controlled_instants_around_minimum_recompute_interval
                .push(
                    read_parent_build_store_path_hash_package_name_from_worker(
                        &fixture,
                        &first_dispatched_job_input_closure_set,
                        DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
                        simulated_worker_read_instant,
                    )
                    .await,
                );
        }
        fixture.db.finish().await;
        assert_eq!(
            package_names_read_by_worker_at_controlled_instants_around_minimum_recompute_interval,
            vec![
                Some("dependency".to_owned()),
                None,
                None,
                Some("reference".to_owned())
            ],
            "Failed lookups right after the first fill trigger another recompute of the set. Failed lookups inside the minimum interval after that recompute skip the database scan. The code can reject references that enter the input closure during that interval. Failed lookups at the end of the interval trigger another recompute of the set. That recompute can return the reference."
        );
    }

    #[tokio::test]
    async fn reject_store_path_outside_input_closure_before_and_after_recompute_triggered_by_miss()
    {
        let fixture = build_stored_dependency_fixture_and_apply_one_sql_change(
            false,
            |shape, _| shape,
            &format!(
                "UPDATE cached_path SET \"references\" = NULL \
                 WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ),
        )
        .await;
        let first_dispatched_job_input_closure_set = StorePathInputClosureHashes::default();
        let recompute_after_miss_instant = Instant::now();
        let mut
        package_names_read_by_worker_at_controlled_instants_around_minimum_recompute_interval =
            Vec::new();
        for simulated_worker_read_instant in [
            recompute_after_miss_instant,
            recompute_after_miss_instant + MIN_INPUT_CLOSURE_SET_RECOMPUTE_INTERVAL,
        ] {
            package_names_read_by_worker_at_controlled_instants_around_minimum_recompute_interval
                .push(
                    read_parent_build_store_path_hash_package_name_from_worker(
                        &fixture,
                        &first_dispatched_job_input_closure_set,
                        DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
                        simulated_worker_read_instant,
                    )
                    .await,
                );
        }
        fixture.db.finish().await;
        assert_eq!(
            package_names_read_by_worker_at_controlled_instants_around_minimum_recompute_interval,
            vec![None, None],
            "Store paths outside the input closure fail the check on the first read and on every recomputation of the job set."
        );
    }

    #[tokio::test]
    async fn readiness_check_worker_read_and_size_lookup_return_stored_output_copy_without_output_signing()
     {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use sea_orm::ActiveEnum;

        for (
            waiting_project_has_read_write_cache_with_private_signing_key,
            sql_run_on_fixture_before_query,
        ) in [
            (false, String::new()),
            (
                true,
                "UPDATE project_cache SET sign_outputs = false".to_owned(),
            ),
            (
                true,
                format!(
                    "UPDATE project_cache SET mode = {write_only}",
                    write_only = CacheSubscriptionMode::WriteOnly.to_value()
                ),
            ),
            (true, "UPDATE cache SET active = false".to_owned()),
            (true, "UPDATE cache SET private_key = ''".to_owned()),
        ] {
            assert_eq!(
                build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(
                    waiting_project_has_read_write_cache_with_private_signing_key,
                    |shape, _| shape,
                    &sql_run_on_fixture_before_query,
                )
                .await,
                ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
                "The ReadWrite cache can't sign output if there's no subscription. It can't sign output if signature is off or if the subscription mode is WriteOnly. The cache also can't sign output if the cache is inactive or if the secret key is empty. Readiness edge check, worker read and size lookup all return the stored copy for direct use. ({sql_run_on_fixture_before_query})"
            );
        }
    }

    async fn build_stored_dependency_fixture_with_write_only_cache_subscription_and_signature_row_for_stored_output_copy()
    -> StorePathContentVariantTrustTestFixture {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use sea_orm::ActiveEnum;

        build_stored_dependency_fixture_and_apply_one_sql_change(
            true,
            |shape, _| shape,
            &format!(
                "UPDATE project_cache SET mode = {write_only}; \
                 INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
                 SELECT uuidv7(), id, (SELECT id FROM cache), decode(repeat('ab', 64), 'hex'), 0, now() \
                 FROM cached_path WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'",
                write_only = CacheSubscriptionMode::WriteOnly.to_value(),
            ),
        )
        .await
    }

    #[tokio::test]
    async fn project_reader_does_not_return_stored_copy_only_direct_use_allows() {
        let fixture = build_stored_dependency_fixture_with_write_only_cache_subscription_and_signature_row_for_stored_output_copy().await;
        let project = ProjectId::new(
            fixture
                .db
                .texts("SELECT project::text FROM project_cache")
                .await[0]
                .parse()
                .expect("a uuid"),
        );
        let rows = highest_ranked_trusted_content_variant_by_store_path_hash_for_project(
            &fixture.db.connection(),
            project,
            &[DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned()],
        )
        .await
        .expect("The selection ran.");
        fixture.db.finish().await;
        assert!(
            rows.is_empty(),
            "The project's reader does not return a copy for direct use only."
        );
    }

    #[tokio::test]
    async fn cache_reader_returns_stored_dependency_output_copy_with_non_null_signature_when_subscription_is_write_only()
     {
        let fixture = build_stored_dependency_fixture_with_write_only_cache_subscription_and_signature_row_for_stored_output_copy().await;
        let rows = get_content_variants_served_by_binary_cache_for_store_path_hashes(
            &fixture.db.connection(),
            fixture.cache,
            &[DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned()],
        )
        .await
        .expect("The selection ran.");
        fixture.db.finish().await;
        assert!(
            rows.contains_key(DEPENDENCY_OUTPUT_STORE_PATH_HASH),
            "The cache can serve stored copies that it signed, even when the only subscription is WriteOnly."
        );
    }

    #[tokio::test]
    async fn build_own_output_checks_reject_stored_copy_allowed_only_by_direct_use() {
        let fixture =
            build_stored_dependency_fixture_and_apply_one_sql_change(false, |shape, _| shape, "")
                .await;
        let parent = fixture.parent;
        let evaluation = EvaluationId::new(
            fixture
                .db
                .texts(&format!(
                    "SELECT evaluation::text FROM build_job WHERE derivation = '{parent}'"
                ))
                .await[0]
                .parse()
                .expect("a uuid"),
        );
        let complete_output_store_path_hashes_reported_for_waiting_evaluation =
            crate::graph::runtime_can_start::complete_output_hashes(
                &fixture.db.connection(),
                evaluation,
                &[DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned()],
            )
            .await
            .expect("The selection ran.");
        let stored_copy_trusted_by_all_live_projects_check_rows = fixture
            .db
            .texts(&format!(
                "SELECT EXISTS (SELECT 1 FROM cached_path v \
                 JOIN derivation_build b ON b.derivation = '{parent}' \
                 WHERE v.hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}' AND {trusted})::text",
                trusted = content_variant_trusted_by_all_projects_with_live_job_on_shared_build(
                    "v.id", "b"
                ),
            ))
            .await;
        let wanted = [(parent, DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned())];
        let own_output_reader_found_stored_copy_row_for_parent_and_dependency_output_hash =
            !highest_ranked_trusted_content_variant_by_derivation_id_and_store_path_hash_for_shared_build_own_outputs(
                &fixture.db.connection(),
                &wanted,
            )
            .await
            .expect("The selection ran.")
            .is_empty();
        let input_size_reader_returns_stored_copy_for_same_pair =
            !find_best_ranked_stored_copy_in_any_cache_by_derivation_id_and_store_path_hash(
                &fixture.db.connection(),
                &wanted,
            )
            .await
            .expect("The selection ran.")
            .is_empty();
        fixture.db.finish().await;
        assert_eq!(
            (
                complete_output_store_path_hashes_reported_for_waiting_evaluation,
                stored_copy_trusted_by_all_live_projects_check_rows,
                own_output_reader_found_stored_copy_row_for_parent_and_dependency_output_hash,
                input_size_reader_returns_stored_copy_for_same_pair,
            ),
            (Vec::<String>::new(), vec!["false".to_owned()], false, true),
            "Direct use can never produce the build's own output. The own output reader can't return the copy that only direct use can allow. The input reader can return that copy."
        );
    }

    #[tokio::test]
    async fn test_stored_copy_is_used_when_readonly_cache_has_unsigned_copy() {
        use gradient_entity::project_cache::CacheSubscriptionMode;
        use sea_orm::ActiveEnum;

        let waiting_project_read_only_cache_id = CacheId::now_v7();
        assert_eq!(
            build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(
                false,
                |shape, _| shape.into_cache(waiting_project_read_only_cache_id),
                &format!(
                    "UPDATE project_cache SET mode = {read_only}, sign_outputs = false; {insert}",
                    read_only = CacheSubscriptionMode::ReadOnly.to_value(),
                    insert = sql_insert_second_dependency_output_store_path_copy_with_nar_hash_and_optional_signature_in_cache(
                        "'sha256:other'",
                        Some((&format!("'{waiting_project_read_only_cache_id}'"), "NULL")),
                    ),
                ),
            )
            .await,
            ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
            "An unsigned copy in the ReadOnly cache doesn't prevent direct use."
        );
    }

    #[tokio::test]
    async fn test_worker_stored_copy_ranks_before_signed_copy_in_cache() {
        assert_eq!(
            build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(
                true,
                |shape, _| shape,
                &sql_insert_second_dependency_output_store_path_copy_with_nar_hash_and_optional_signature_in_cache(
                    "'sha256:other'",
                    Some(("(SELECT id FROM cache)", "decode(repeat('ab', 64), 'hex')")),
                ),
            )
            .await,
            ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
            "The copy that a worker built can rank before the signed copy in the cache that the project can read."
        );
    }

    #[tokio::test]
    async fn fixed_output_input_and_closure_reference_are_used_directly_without_signing_cache_and_all_readers_return_stored_copy()
     {
        for fixed_output_store_path_hash in [
            DEPENDENCY_OUTPUT_STORE_PATH_HASH,
            DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
        ] {
            assert_eq!(
                build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(
                    false,
                    |shape, _| {
                        shape
                            .derivation(&format!("/nix/store/{}-source.drv", "r".repeat(32)))
                            .output(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH)
                    },
                    &sql_that_marks_derivation_as_fixed_output(fixed_output_store_path_hash),
                )
                .await,
                ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
                "Projects with no signing cache use a fixed-output download from another project directly, like every other input ({fixed_output_store_path_hash})."
            );
        }
    }

    #[tokio::test]
    async fn two_stored_input_copies_make_build_readers_pick_same_copy() {
        assert_eq!(
            build_stored_dependency_fixture_run_sql_and_return_three_dependency_output_reader_answers(
                false,
                |shape, _| shape,
                &sql_insert_second_dependency_output_store_path_copy_with_nar_hash_and_optional_signature_in_cache(
                    "'sha256:other'",
                    None,
                ),
            )
            .await,
            ParentBuildStorePathThreeReaderAnswers::all_three_readers_return_stored_copy_for_package("dependency"),
            "Both build readers choose the same copy of the input."
        );
    }
}
