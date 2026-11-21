/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::store_path_content_producer::StoredContentVariantProducerKind;
use gradient_types::*;
use sea_orm::{
    ActiveEnum, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, FromQueryResult, JoinType,
    QueryFilter, QuerySelect, RelationTrait, Value,
};
use std::collections::HashSet;

crate::sql_fn! {
    CACHES_ALLOWED_TO_SIGN_COMMITTED_STORE_PATH_CONTENT_VARIANT = || format!(
        "SELECT target.id AS cache, \
                ($2::uuid IS NULL OR EXISTS (SELECT 1 FROM project_cache signature_requesting_subscription \
                  WHERE signature_requesting_subscription.project = $2::uuid \
                    AND signature_requesting_subscription.cache = target.id AND {signing})) AS sign_allowed \
         FROM cache target \
         WHERE target.id = ANY($3::uuid[]) AND target.active \
           AND NOT EXISTS (SELECT 1 FROM cached_path_signature held \
                           JOIN cached_path committed ON committed.id = $1 \
                           WHERE held.cache = target.id AND held.store_path_hash = committed.hash \
                             AND held.cached_path <> committed.id) \
           AND ($4::bool OR {uploaded} OR {trusted}) \
         ORDER BY target.id",
        signing = signing_subscription("signature_requesting_subscription"),
        uploaded = uploaded_into_cache("$1", "target.id"),
        trusted = project_trusts_content_variant_exists_sql("$1", "$2::uuid"),
    ),
        params = [CachedPathId, ProjectId, CacheIds(4), Bool(false)];

    CACHES_WHOSE_PROJECT_SUBSCRIPTION_ASKS_FOR_SIGNATURES = || format!(
        "SELECT subscription.cache FROM project_cache subscription \
         WHERE subscription.project = $1 AND {signing} \
         ORDER BY subscription.cache",
        signing = signing_subscription("subscription"),
    ),
        params = [ProjectId];

    PROJECT_CACHE_ROW_HAS_ACTIVE_READ_WRITE_SUBSCRIPTION_THAT_SIGNS_OUTPUTS_AND_CACHE_HAS_PRIVATE_SIGNING_KEY_SQL = || format!(
        "SELECT EXISTS (SELECT 1 FROM project_cache subscription WHERE {signing}) AS can_sign",
        signing = project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key(
            "subscription"
        ),
    ),
        params = [];

    INSERT_PLACEHOLDERS_FOR_PROJECT_OUTPUTS_INTO_CACHE = || unsigned_placeholders_insert(
        "SELECT DISTINCT dout.hash, t.project \
         FROM task t \
         JOIN evaluation e           ON e.task = t.id \
         JOIN build_job bj           ON bj.evaluation = e.id \
         JOIN derivation_output dout ON dout.derivation = bj.derivation \
         WHERE t.project = $1 AND dout.hash IS NOT NULL",
        "claimable.cache = $2",
    ),
        params = [ProjectId, CacheId],
        tier = Sweep,
        budget = crate::sql::Budget::sweep().buffers(1_500_000)
            .because("The background pass of signature-backfill can run this one-time backfill after the cache subscription starts the signature. \
                      This statement can write one unsigned placeholder per project output into the cache. \
                      Buffer reads reach 1.19 million on the amplified dump. \
                      The cost is about 1.3 seconds per 100k output hashes.");

    INSERT_PLACEHOLDERS_FOR_PROJECT_STORE_HASHES = || unsigned_placeholders_insert(
        "SELECT DISTINCT pushed.hash, $1::uuid FROM unnest($2::text[]) pushed(hash)",
        "true",
    ),
        params = [ProjectId, CachedPathHashes(64)],
        tier = Bulk,
        budget = crate::sql::Budget::bulk().buffers(400_000)
            .because("A single push can require the reference closure for every pushed path. \
                      Each closure path can cost about 20 buffers to select one trusted variant.");
}

fn unsigned_placeholders_insert(seeds: &str, filter: &str) -> String {
    format!(
        "INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
         SELECT uuidv7(), claimable.cached_path, claimable.cache, 0, (now() AT TIME ZONE 'UTC') \
         FROM ({claimable}) claimable WHERE {filter} \
         ON CONFLICT DO NOTHING",
        claimable =
            build_sql_for_content_variants_needing_signature_from_store_hash_project_seeds(seeds),
    )
}

pub async fn insert_unsigned_signature_placeholders_for_trusted_store_path_content_variants<
    C: ConnectionTrait,
>(
    db: &C,
    project: ProjectId,
    cache: CacheId,
) -> Result<u64, DbErr> {
    Ok(db
        .execute_raw(INSERT_PLACEHOLDERS_FOR_PROJECT_OUTPUTS_INTO_CACHE.bind([
            Value::Uuid(Some(project.into_inner())),
            Value::Uuid(Some(cache.into_inner())),
        ]))
        .await?
        .rows_affected())
}

pub const SUBSCRIPTION_SIGNATURE_BACKFILL_ADVISORY_LOCK_CLASS: i32 = 647;

crate::sql! {
    SELECT_PROJECT_CACHE_ROWS_THAT_OWE_SIGNATURE_BACKFILL_AFTER_CURSOR_OLDEST_FIRST_WITH_LIMIT =
        "SELECT id, project, cache, unsigned_signature_backfill_requested_at FROM project_cache \
         WHERE unsigned_signature_backfill_requested_at IS NOT NULL \
           AND ($1::timestamp IS NULL \
                OR (unsigned_signature_backfill_requested_at, id) > ($1::timestamp, $2::uuid)) \
         ORDER BY unsigned_signature_backfill_requested_at, id \
         LIMIT $3",
        params = [Now, NewUuid, Int(100)],
        tier = Sweep;

    CLEAR_SUBSCRIPTION_BACKFILL_REQUESTED_TIME_IF_STILL_EQUALS_READ_TIME_SQL =
        "UPDATE project_cache SET unsigned_signature_backfill_requested_at = NULL \
         WHERE id = $1 AND unsigned_signature_backfill_requested_at = $2",
        params = [NewUuid, Now],
        tier = Hot;
}

crate::sql_fn! {
    SUBSCRIPTION_BACKFILL_LOCK_TRY = || format!(
        "SELECT pg_try_advisory_xact_lock({SUBSCRIPTION_SIGNATURE_BACKFILL_ADVISORY_LOCK_CLASS}, hashtext($1::text)) AS locked"
    ),
        params = [NewUuid],
        tier = Hot;
}

#[derive(Clone, Debug, PartialEq, Eq, FromQueryResult)]
pub struct PendingSignatureBackfillSubscription {
    pub id: gradient_types::ids::ProjectCacheId,
    pub project: ProjectId,
    pub cache: CacheId,
    pub unsigned_signature_backfill_requested_at: sea_orm::prelude::DateTime,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackfillRowsWrittenOrSkippedForLockHeldElsewhere {
    BackfillWrotePlaceholderRows(u64),
    SkippedBecauseAnotherServerHoldsSubscriptionLock,
}

pub async fn read_subscriptions_owing_signature_backfill_after_cursor<C: ConnectionTrait>(
    db: &C,
    after: Option<&PendingSignatureBackfillSubscription>,
    limit: u64,
) -> Result<Vec<PendingSignatureBackfillSubscription>, DbErr> {
    PendingSignatureBackfillSubscription::find_by_statement(
        SELECT_PROJECT_CACHE_ROWS_THAT_OWE_SIGNATURE_BACKFILL_AFTER_CURSOR_OLDEST_FIRST_WITH_LIMIT
            .bind([
                after
                    .map(|owed| owed.unsigned_signature_backfill_requested_at)
                    .into(),
                after.map(|owed| owed.id.into_inner()).into(),
                i64::try_from(limit).unwrap_or(i64::MAX).into(),
            ]),
    )
    .all(db)
    .await
}

pub async fn write_unsigned_signature_placeholders_for_project_earlier_outputs_then_clear_owed_subscription_requested_time_in_locked_transaction_and_commit<
    C: sea_orm::TransactionTrait<Transaction = sea_orm::DatabaseTransaction>,
>(
    db: &C,
    owed: &PendingSignatureBackfillSubscription,
) -> Result<BackfillRowsWrittenOrSkippedForLockHeldElsewhere, DbErr> {
    let transaction = db.begin().await?;
    let locked: bool = SUBSCRIPTION_BACKFILL_LOCK_TRY
        .scalar(&transaction, [owed.id.into_inner().into()], "locked")
        .await?;
    if !locked {
        transaction.rollback().await?;
        return Ok(
            BackfillRowsWrittenOrSkippedForLockHeldElsewhere::SkippedBecauseAnotherServerHoldsSubscriptionLock,
        );
    }
    let written = insert_unsigned_signature_placeholders_for_trusted_store_path_content_variants(
        &transaction,
        owed.project,
        owed.cache,
    )
    .await?;
    transaction
        .execute_raw(
            CLEAR_SUBSCRIPTION_BACKFILL_REQUESTED_TIME_IF_STILL_EQUALS_READ_TIME_SQL.bind([
                owed.id.into_inner().into(),
                owed.unsigned_signature_backfill_requested_at.into(),
            ]),
        )
        .await?;
    transaction.commit().await?;
    Ok(BackfillRowsWrittenOrSkippedForLockHeldElsewhere::BackfillWrotePlaceholderRows(written))
}

pub async fn insert_unsigned_signature_placeholders_for_trusted_content_variants<
    C: ConnectionTrait,
>(
    db: &C,
    project: ProjectId,
    hashes: &[String],
) -> Result<u64, DbErr> {
    if hashes.is_empty() {
        return Ok(0);
    }

    Ok(db
        .execute_raw(INSERT_PLACEHOLDERS_FOR_PROJECT_STORE_HASHES.bind([
            Value::Uuid(Some(project.into_inner())),
            hashes.to_vec().into(),
        ]))
        .await?
        .rows_affected())
}

pub async fn has_project_cache_subscription_for_stored_input_signing<C: ConnectionTrait>(
    db: &C,
) -> Result<bool, DbErr> {
    PROJECT_CACHE_ROW_HAS_ACTIVE_READ_WRITE_SUBSCRIPTION_THAT_SIGNS_OUTPUTS_AND_CACHE_HAS_PRIVATE_SIGNING_KEY_SQL
        .scalar(db, [], "can_sign")
        .await
}

fn kind(kind: StoredContentVariantProducerKind) -> i32 {
    kind.to_value()
}

fn mode(mode: CacheSubscriptionMode) -> i32 {
    mode.to_value()
}

pub fn signing_subscription(subscription: &str) -> String {
    format!(
        "{subscription}.mode IN ({read_write}, {write_only}) AND {subscription}.sign_outputs \
         AND EXISTS (SELECT 1 FROM cache signing_cache WHERE signing_cache.id = {subscription}.cache AND signing_cache.active)",
        read_write = mode(CacheSubscriptionMode::ReadWrite),
        write_only = mode(CacheSubscriptionMode::WriteOnly),
    )
}

pub(crate) fn reading_subscription(project: &str) -> String {
    format!(
        "SELECT 1 FROM project_cache trust_read JOIN cache trust_read_cache ON trust_read_cache.id = trust_read.cache \
         WHERE trust_read.project = {project} AND trust_read.mode <> {write_only} AND trust_read_cache.active",
        write_only = mode(CacheSubscriptionMode::WriteOnly),
    )
}

fn uploaded_into_cache(variant: &str, cache: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM store_path_content_producer trust_upload \
         WHERE trust_upload.cached_path = {variant} AND trust_upload.kind = {upload} AND trust_upload.cache = {cache})",
        upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
    )
}

fn content_has_signed_cache_backfill_producer_and_cached_signature_for_content(
    variant: &str,
    cache: &str,
) -> String {
    format!(
        "({backfill} AND EXISTS (SELECT 1 FROM cached_path_signature trust_held \
         WHERE trust_held.cached_path = {variant} AND trust_held.cache = {cache} \
           AND coalesce(trust_held.signature, trust_held.previous_signing_key_signature) IS NOT NULL))",
        backfill = has_producer_kind(
            variant,
            StoredContentVariantProducerKind::SignedCacheBackfill
        ),
    )
}

pub(crate) fn has_producer_kind(
    variant: &str,
    producer_kind: StoredContentVariantProducerKind,
) -> String {
    format!(
        "EXISTS (SELECT 1 FROM store_path_content_producer trust_kind \
         WHERE trust_kind.cached_path = {variant} AND trust_kind.kind = {kind})",
        kind = kind(producer_kind),
    )
}

pub fn project_trusts_worker_credentials_sql(
    worker_registration: &str,
    team_worker: &str,
    project: &str,
) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM worker_registration trust_registration \
                  WHERE trust_registration.id = {worker_registration} \
                    AND trust_registration.peer_id = {project}) \
          OR EXISTS (SELECT 1 FROM team_worker trust_team_worker \
                     JOIN team_project trust_grant ON trust_grant.team = trust_team_worker.team \
                     WHERE trust_team_worker.id = {team_worker} \
                       AND trust_grant.includes_workers AND trust_grant.project = {project}))"
    )
}

fn project_reads_signed_cache_content_variant_exists_sql(project: &str, variant: &str) -> String {
    format!(
        "EXISTS ({reads} AND trust_read.cache IN ( \
           SELECT trust_signed.cache FROM cached_path_signature trust_signed \
           WHERE trust_signed.cached_path = {variant} \
             AND coalesce(trust_signed.signature, trust_signed.previous_signing_key_signature) IS NOT NULL))",
        reads = reading_subscription(project),
    )
}

fn project_reads_cache_content_variant_with_any_signature_exists_sql(
    project: &str,
    variant: &str,
    read_cache_rows_counted_for_content_variant_trust_sql_condition: Option<&str>,
) -> String {
    format!(
        "EXISTS ({reads} AND trust_read.cache IN ( \
           SELECT trust_held_by_read_cache.cache FROM cached_path_signature trust_held_by_read_cache \
           WHERE trust_held_by_read_cache.cached_path = {variant}{counted}))",
        reads = reading_subscription(project),
        counted = read_cache_rows_counted_for_content_variant_trust_sql_condition
            .map(|condition| format!(" AND {condition}"))
            .unwrap_or_default(),
    )
}

fn project_trusts_content_producer_sql(producer: &str, project: &str) -> String {
    let reads = reading_subscription(project);
    format!(
        "(({producer}.kind = {worker} AND {worker_trusted}) \
           OR ({producer}.kind = {upload} AND EXISTS ({reads} AND trust_read.cache = {producer}.cache)) \
           OR ({producer}.kind = {member_upload} AND {producer}.uploading_member_project_id = {project}) \
           OR ({producer}.kind = {before} AND {active_cache_signed_content_variant_reads}))",
        active_cache_signed_content_variant_reads =
            project_reads_signed_cache_content_variant_exists_sql(
                project,
                &format!("{producer}.cached_path"),
            ),
        worker = kind(StoredContentVariantProducerKind::BuiltInDispatchedBuildJob),
        worker_trusted = project_trusts_worker_credentials_sql(
            &format!("{producer}.worker_registration"),
            &format!("{producer}.team_worker"),
            project,
        ),
        upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
        member_upload = kind(StoredContentVariantProducerKind::UploadedByProjectMember),
        before = kind(StoredContentVariantProducerKind::SignedCacheBackfill),
    )
}

pub fn project_trusts_content_variant_exists_sql(variant: &str, project: &str) -> String {
    build_sql_test_for_project_trusts_content_variant_with_read_cache_row_condition(
        variant, project, None,
    )
}

fn build_sql_test_for_project_trusts_content_variant_with_read_cache_row_condition(
    variant: &str,
    project: &str,
    read_cache_rows_counted_for_content_variant_trust_sql_condition: Option<&str>,
) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM store_path_content_producer trust_producer \
          WHERE trust_producer.cached_path = {variant} AND {trusted}) \
          OR {cache_project_reads_holds_row_for_copy})",
        trusted = project_trusts_content_producer_sql("trust_producer", project),
        cache_project_reads_holds_row_for_copy =
            project_reads_cache_content_variant_with_any_signature_exists_sql(
                project,
                variant,
                read_cache_rows_counted_for_content_variant_trust_sql_condition,
            ),
    )
}

pub fn cache_trusts_content_producer_sql(producer: &str, cache: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM project_cache accepting \
          WHERE accepting.cache = {cache} AND {signing} AND {trusted})",
        signing = signing_subscription("accepting"),
        trusted = project_trusts_content_producer_sql(producer, "accepting.project"),
    )
}

fn project_reads_content_variant_as_signed_from_read_write_cache_with_private_signing_key_sql_condition(
    variant: &str,
    cache: &str,
) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM project_cache signed_reader \
                  WHERE signed_reader.cache = {cache} AND {target}) \
          AND EXISTS (SELECT 1 FROM cached_path_signature reader_signed \
                      WHERE reader_signed.cached_path = {variant} AND reader_signed.cache = {cache} \
                        AND coalesce(reader_signed.signature, reader_signed.previous_signing_key_signature) IS NOT NULL))",
        target = project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key(
            "signed_reader"
        ),
    )
}

pub fn cache_can_sign_content_variant_sql_condition(variant: &str, cache: &str) -> String {
    format!(
        "({uploaded} OR {backfilled} OR {read_as_signed} \
          OR EXISTS (SELECT 1 FROM store_path_content_producer trust_producer \
          WHERE trust_producer.cached_path = {variant} AND {trusted}))",
        uploaded = uploaded_into_cache(variant, cache),
        backfilled = content_has_signed_cache_backfill_producer_and_cached_signature_for_content(
            variant, cache
        ),
        read_as_signed =
            project_reads_content_variant_as_signed_from_read_write_cache_with_private_signing_key_sql_condition(
                variant, cache
            ),
        trusted = cache_trusts_content_producer_sql("trust_producer", cache),
    )
}

pub(crate) fn variant_rank(variant: &str, cache: &str) -> String {
    format!(
        "{uploaded} DESC, {worker} DESC, {variant}.created_at, {variant}.id",
        uploaded = uploaded_into_cache(&format!("{variant}.id"), cache),
        worker = has_producer_kind(
            &format!("{variant}.id"),
            StoredContentVariantProducerKind::BuiltInDispatchedBuildJob
        ),
    )
}

pub fn content_variant_is_readable_by_project_sql(
    variant: &str,
    project: &str,
    signing_cache: &str,
) -> String {
    format!(
        "({worker} OR {trusted})",
        worker = has_producer_kind(
            variant,
            StoredContentVariantProducerKind::BuiltInDispatchedBuildJob
        ),
        trusted = build_sql_test_for_project_trusts_content_variant_with_read_cache_row_condition(
            variant,
            project,
            Some(&format!(
                "(trust_held_by_read_cache.cache <> {signing_cache} \
                  OR coalesce(trust_held_by_read_cache.signature, trust_held_by_read_cache.previous_signing_key_signature) IS NOT NULL)"
            )),
        ),
    )
}

fn sql_prefix_for_project_readable_store_path_content_variant_with_nar_hash_and_nar_size(
    project: &str,
    cache: &str,
) -> String {
    format!(
        "{readable} AND candidate.nar_hash IS NOT NULL AND candidate.nar_size IS NOT NULL AND ",
        readable = content_variant_is_readable_by_project_sql("candidate.id", project, cache),
    )
}

fn candidate_content_variant_is_in_target_cache_or_trusted_by_project(
    project: &str,
    cache: &str,
) -> String {
    format!(
        "({uploaded} OR {trusted}) AND ",
        uploaded = uploaded_into_cache("candidate.id", cache),
        trusted = project_trusts_content_variant_exists_sql("candidate.id", project),
    )
}

pub fn project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key(
    subscription: &str,
) -> String {
    format!(
        "({signing} AND EXISTS ({reads} AND trust_read.id = {subscription}.id) \
          AND EXISTS (SELECT 1 FROM cache cache_row_with_nonempty_private_signing_key \
                      WHERE cache_row_with_nonempty_private_signing_key.id = {subscription}.cache AND cache_row_with_nonempty_private_signing_key.private_key <> ''))",
        signing = signing_subscription(subscription),
        reads = reading_subscription(&format!("{subscription}.project")),
    )
}

fn build_recursive_store_path_content_variant_trust_sql_text(
    shared_build_derivation: &str,
) -> String {
    format!(
        r#"WITH RECURSIVE shared_build_input_closure_store_path_hashes(hash) AS (
    SELECT direct_dependency_outputs_used_as_inputs.hash
    FROM derivation_dependency direct_dependencies_of_shared_build_derivation
    JOIN derivation_output direct_dependency_outputs_used_as_inputs
      ON direct_dependency_outputs_used_as_inputs.derivation = direct_dependencies_of_shared_build_derivation.dependency
     AND (direct_dependencies_of_shared_build_derivation.dependency_outputs IS NULL
          OR direct_dependency_outputs_used_as_inputs.name = ANY(direct_dependencies_of_shared_build_derivation.dependency_outputs))
    WHERE direct_dependencies_of_shared_build_derivation.derivation = {shared_build_derivation}
      AND direct_dependencies_of_shared_build_derivation.kind IN (0, 2)
      AND direct_dependency_outputs_used_as_inputs.hash IS NOT NULL
    UNION
    SELECT {STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION}
    FROM shared_build_input_closure_store_path_hashes
    JOIN cached_path cached_path_references_to_walk
      ON cached_path_references_to_walk.hash = shared_build_input_closure_store_path_hashes.hash
    CROSS JOIN LATERAL unnest(string_to_array(cached_path_references_to_walk."references", ' ')) r(token)
    WHERE r.token <> ''
)
SELECT hash FROM shared_build_input_closure_store_path_hashes"#
    )
}

crate::sql_fn! {
    SHARED_BUILD_INPUT_CLOSURE_STORE_PATH_HASHES_QUERY = || {
        build_recursive_store_path_content_variant_trust_sql_text(
            "(SELECT derivation FROM derivation_build WHERE id = $1::uuid)",
        )
    },
        params = [SharedBuildId],
        tier = Walk,
        flags = [Walk];
}

pub(crate) async fn query_input_closure_store_path_hashes_for_build<C: ConnectionTrait>(
    db: &C,
    shared_build: DerivationBuildId,
) -> Result<HashSet<String>, DbErr> {
    let rows = db
        .query_all_raw(
            SHARED_BUILD_INPUT_CLOSURE_STORE_PATH_HASHES_QUERY
                .bind([Value::Uuid(Some(shared_build.into_inner()))]),
        )
        .await?;
    rows.iter()
        .map(|row| row.try_get::<String>("", "hash"))
        .collect()
}

fn seed_missing_signed_dependency_output_cache_rows_for_project_with_live_parent_job_and_signing_cache_subscription(
    parent_build_filter: &str,
) -> String {
    format!(
        "SELECT DISTINCT output_blocked_by_untrusted_variant.hash, project_with_live_build_job.project, parent.derivation AS parent \
         FROM derivation_build parent \
         JOIN derivation_dependency e ON e.derivation = parent.derivation AND e.kind IN (0, 2) \
         JOIN derivation_build dep ON dep.derivation = e.dependency \
         JOIN derivation_output output_blocked_by_untrusted_variant ON output_blocked_by_untrusted_variant.derivation = e.dependency \
          AND output_blocked_by_untrusted_variant.hash IS NOT NULL \
          AND (e.dependency_outputs IS NULL OR output_blocked_by_untrusted_variant.name = ANY(e.dependency_outputs)) \
         CROSS JOIN LATERAL (SELECT DISTINCT live_task.project {live_jobs}) project_with_live_build_job \
         WHERE {parent_build_filter} AND dep.fetchable AND NOT {unneeded} \
           AND EXISTS (SELECT 1 FROM project_cache unsigned_signing_project_cache_for_waiting_project \
                       WHERE unsigned_signing_project_cache_for_waiting_project.project = project_with_live_build_job.project \
                         AND {usable_signing_target} \
                         AND NOT EXISTS (SELECT 1 FROM cached_path_signature dependency_output_already_published_in_target_cache \
                                         WHERE dependency_output_already_published_in_target_cache.cache = unsigned_signing_project_cache_for_waiting_project.cache \
                                           AND dependency_output_already_published_in_target_cache.store_path_hash = output_blocked_by_untrusted_variant.hash \
                                           AND dependency_output_already_published_in_target_cache.signature IS NOT NULL) \
                         AND EXISTS ({publishable_copy}))",
        live_jobs = crate::graph::predicates::not_aborted_build_jobs_with_unfinished_or_recently_finished_evaluation_sql_for_derivation_build("parent"),
        unneeded = crate::graph::predicates::builds_with_unneeded_output_predicate("e", "dep"),
        usable_signing_target =
            project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key(
                "unsigned_signing_project_cache_for_waiting_project"
            ),
        publishable_copy = chosen_variant(
            "output_blocked_by_untrusted_variant.hash",
            "project_with_live_build_job.project",
            "unsigned_signing_project_cache_for_waiting_project.cache",
            sql_prefix_for_project_readable_store_path_content_variant_with_nar_hash_and_nar_size,
        ),
    )
}

pub(crate) fn select_parent_build_derivation_ids_with_missing_dependency_output_signature_publish_work(
    parent_build_filter: &str,
) -> String {
    format!(
        "SELECT DISTINCT seeded.parent FROM ({seeds}) seeded",
        seeds = seed_missing_signed_dependency_output_cache_rows_for_project_with_live_parent_job_and_signing_cache_subscription(
            parent_build_filter
        ),
    )
}

fn build_parent_builds_with_unsigned_dependency_outputs_query(filter: &str) -> String {
    format!(
        "SELECT p.derivation FROM derivation_build p \
         WHERE p.status IN ({useful}) \
           AND EXISTS (SELECT 1 {live_jobs}) \
           AND EXISTS ({parents_with_publish_work}) \
           AND {filter}",
        useful = crate::sql::status::build_in(&[
            gradient_entity::build::BuildStatus::Created,
            gradient_entity::build::BuildStatus::Queued,
            gradient_entity::build::BuildStatus::Building,
            gradient_entity::build::BuildStatus::FailedTransient,
            gradient_entity::build::BuildStatus::Completed,
        ]),
        live_jobs = crate::graph::predicates::not_aborted_build_jobs_with_unfinished_or_recently_finished_evaluation_sql_for_derivation_build("p"),
        parents_with_publish_work =
            select_parent_build_derivation_ids_with_missing_dependency_output_signature_publish_work(
                "parent.id = p.id"
            ),
    )
}

crate::sql_fn! {
    SQL_LIST_PARENT_BUILD_DERIVATION_IDS_WITH_DEPENDENCY_OUTPUTS_TO_SIGN_PAGE = || {
        build_parent_builds_with_unsigned_dependency_outputs_query(
            "p.derivation > $1 AND p.derivation <= $2 ORDER BY p.derivation LIMIT $3",
        )
    },
        params = [DerivationId, DerivationId, Int(1000)],
        tier = Bulk;
}

pub async fn list_parent_build_derivation_ids_with_dependency_outputs_to_sign_page<
    C: ConnectionTrait,
>(
    db: &C,
    after: DerivationId,
    upto: DerivationId,
    limit: usize,
) -> Result<Vec<DerivationId>, DbErr> {
    Ok(crate::graph::promotion::returned_derivations(
        db.query_all_raw(
            SQL_LIST_PARENT_BUILD_DERIVATION_IDS_WITH_DEPENDENCY_OUTPUTS_TO_SIGN_PAGE.bind([
                after.into_inner().into(),
                upto.into_inner().into(),
                (limit as i64).into(),
            ]),
        )
        .await?,
    ))
}

pub(crate) const STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION: &str =
    "split_part(regexp_replace(r.token, '^/nix/store/', ''), '-', 1)";

crate::sql_fn! {
    pub SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING = || format!(
        r#"
WITH RECURSIVE signable_rows_for_blocked_dependents AS ({claimable}),
signature_row_references_that_may_be_signed(cache, hash, cached_path_has_signature, referenced_hash) AS MATERIALIZED (
    SELECT signable.cache, signable.hash, signable.cached_path_has_signature,
           {STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION}
    FROM signable_rows_for_blocked_dependents signable
    CROSS JOIN LATERAL unnest(string_to_array(signable."references", ' ')) r(token)
    WHERE r.token <> ''
),
cache_store_path_pairs_to_drop(cache, hash) AS (
    SELECT reference.cache, reference.hash
    FROM signature_row_references_that_may_be_signed reference
    WHERE reference.referenced_hash <> reference.hash
      AND NOT reference.cached_path_has_signature
      AND NOT EXISTS (SELECT 1 FROM signable_rows_for_blocked_dependents covered
                      WHERE covered.cache = reference.cache AND covered.hash = reference.referenced_hash)
      AND NOT EXISTS (SELECT 1 FROM cached_path_signature s
                      WHERE s.cache = reference.cache AND s.store_path_hash = reference.referenced_hash
                        AND s.signature IS NOT NULL)
    UNION
    SELECT reference.cache, reference.hash
    FROM signature_row_references_that_may_be_signed reference
    JOIN cache_store_path_pairs_to_drop dropped
      ON dropped.cache = reference.cache AND dropped.hash = reference.referenced_hash
    WHERE NOT reference.cached_path_has_signature
),
signature_rows_for_dropped_store_path_cache_pairs(cache, hash) AS MATERIALIZED (
    SELECT DISTINCT reference.cache, reference.hash
    FROM signature_row_references_that_may_be_signed reference
    JOIN cache_store_path_pairs_to_drop dropped
      ON dropped.cache = reference.cache AND dropped.hash = reference.referenced_hash
)
SELECT signable.cached_path, signable.hash, signable.package, signable.nar_hash, signable.nar_size,
       signable."references", signable.cache, signable.cached_path_has_signature,
       signable.all_referenced_paths_are_stored AND referencing.hash IS NULL AS all_referenced_paths_are_stored
FROM signable_rows_for_blocked_dependents signable
LEFT JOIN signature_rows_for_dropped_store_path_cache_pairs referencing
  ON referencing.cache = signable.cache AND referencing.hash = signable.hash
WHERE NOT EXISTS (SELECT 1 FROM cache_store_path_pairs_to_drop m
                  WHERE m.cache = signable.cache AND m.hash = signable.hash)
ORDER BY signable.cache, signable.hash
"#,
        claimable = build_recursive_sql_for_content_variant_to_sign_with_reference_closure_per_cache_and_store_path_hash(
            &format!(
                "SELECT DISTINCT blocked.hash, blocked.project FROM ({seeds}) blocked",
                seeds = seed_missing_signed_dependency_output_cache_rows_for_project_with_live_parent_job_and_signing_cache_subscription(
                    "parent.derivation = ANY($1::uuid[])"
                ),
            ),
            &project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key(
                "claim_subscription"
            ),
            sql_prefix_for_project_readable_store_path_content_variant_with_nar_hash_and_nar_size,
        ),
    ),
        params = [DerivationIds(64)],
        tier = Sweep;
}

#[derive(Debug, Clone, PartialEq, Eq, FromQueryResult)]
pub struct StorePathContentVariantSignatureTargetRow {
    pub cached_path: CachedPathId,
    pub hash: String,
    pub package: String,
    pub nar_hash: Option<String>,
    pub nar_size: Option<i64>,
    pub references: Option<String>,
    pub cache: CacheId,
}

pub async fn load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches<
    C: ConnectionTrait,
>(
    db: &C,
    parents: &[DerivationId],
) -> Result<Vec<StorePathContentVariantSignatureTargetRow>, DbErr> {
    if parents.is_empty() {
        return Ok(Vec::new());
    }
    let parents: Vec<uuid::Uuid> = parents.iter().map(|p| p.into_inner()).collect();
    StorePathContentVariantSignatureTargetRow::find_by_statement(
        SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING
            .bind([parents.into()]),
    )
    .all(db)
    .await
}

fn chosen_variant(
    hash: &str,
    project: &str,
    cache: &str,
    eligible: fn(&str, &str) -> String,
) -> String {
    format!(
        "SELECT candidate.id FROM cached_path candidate \
         WHERE candidate.hash = {hash} \
           AND {eligible}(EXISTS (SELECT 1 FROM cached_path_signature held \
                        WHERE held.cached_path = candidate.id AND held.cache = {cache}) \
                OR NOT EXISTS (SELECT 1 FROM cached_path_signature held \
                               WHERE held.store_path_hash = candidate.hash AND held.cache = {cache})) \
         ORDER BY {rank} LIMIT 1",
        eligible = eligible(project, cache),
        rank = variant_rank("candidate", cache),
    )
}

pub fn build_sql_for_content_variants_needing_signature_from_store_hash_project_seeds(
    seeds: &str,
) -> String {
    build_recursive_sql_for_content_variant_to_sign_with_reference_closure_per_cache_and_store_path_hash(
        seeds,
        &signing_subscription("claim_subscription"),
        candidate_content_variant_is_in_target_cache_or_trusted_by_project,
    )
}

fn build_recursive_sql_for_content_variant_to_sign_with_reference_closure_per_cache_and_store_path_hash(
    seeds: &str,
    target_subscription: &str,
    eligible: fn(&str, &str) -> String,
) -> String {
    format!(
        r#"
WITH RECURSIVE claim_seed(hash, project) AS ({seeds}),
claim_target AS (
    SELECT DISTINCT claim_seed.hash, claim_seed.project, claim_subscription.cache
    FROM claim_seed
    JOIN project_cache claim_subscription ON claim_subscription.project = claim_seed.project
    WHERE {target_subscription}
      AND NOT EXISTS (
          SELECT 1 FROM cached_path_signature target_cache_cached_path_signature_with_signature_and_references_claimed
          WHERE target_cache_cached_path_signature_with_signature_and_references_claimed.cache = claim_subscription.cache
            AND target_cache_cached_path_signature_with_signature_and_references_claimed.store_path_hash = claim_seed.hash
            AND target_cache_cached_path_signature_with_signature_and_references_claimed.signature IS NOT NULL
            AND target_cache_cached_path_signature_with_signature_and_references_claimed.references_are_claimed)
),
claim(cached_path, project, cache) AS (
    SELECT chosen.id, claim_target.project, claim_target.cache
    FROM claim_target
    CROSS JOIN LATERAL ({seed_choice}) chosen
    UNION
    SELECT chosen.id, reference.project, reference.cache
    FROM (
        SELECT DISTINCT {STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION} AS hash,
               claim.project, claim.cache
        FROM claim
        JOIN cached_path claimed ON claimed.id = claim.cached_path
        CROSS JOIN LATERAL unnest(string_to_array(claimed."references", ' ')) r(token)
        WHERE r.token <> ''
          AND NOT EXISTS (
              SELECT 1 FROM cached_path_signature s
              WHERE s.cached_path = claimed.id AND s.cache = claim.cache
                AND s.signature IS NOT NULL AND s.references_are_claimed)
    ) reference
    CROSS JOIN LATERAL ({reference_choice}) chosen
),
selected_content_variant_for_signing AS MATERIALIZED (
    SELECT DISTINCT ON (claim.cache, cp.hash)
           cp.id AS cached_path, cp.hash, cp.package, cp.nar_hash, cp.nar_size, cp."references",
           claim.cache,
           coalesce(s.signature IS NOT NULL, false) AS cached_path_has_signature
    FROM claim
    JOIN cached_path cp ON cp.id = claim.cached_path
    LEFT JOIN cached_path_signature s ON s.cached_path = cp.id AND s.cache = claim.cache
    WHERE NOT coalesce(s.signature IS NOT NULL AND s.references_are_claimed, false)
    ORDER BY claim.cache, cp.hash, {rank}
),
store_paths_to_claim(cache, hash) AS MATERIALIZED (
    SELECT DISTINCT claim.cache, claimed_copy.hash
    FROM claim JOIN cached_path claimed_copy ON claimed_copy.id = claim.cached_path
),
references_of_selected_content_variant_for_signing(cache, hash, referenced_hash) AS MATERIALIZED (
    SELECT selected_content_variant_for_signing.cache, selected_content_variant_for_signing.hash, {STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION}
    FROM selected_content_variant_for_signing
    CROSS JOIN LATERAL unnest(string_to_array(selected_content_variant_for_signing."references", ' ')) r(token)
    WHERE r.token <> ''
),
selected_content_variant_for_signing_with_unstored_reference(cache, hash) AS MATERIALIZED (
    SELECT DISTINCT reference.cache, reference.hash
    FROM references_of_selected_content_variant_for_signing reference
    WHERE reference.referenced_hash <> reference.hash
      AND NOT EXISTS (SELECT 1 FROM store_paths_to_claim claimed
                      WHERE claimed.cache = reference.cache AND claimed.hash = reference.referenced_hash)
      AND NOT EXISTS (SELECT 1 FROM cached_path_signature referenced_signature
                      WHERE referenced_signature.cache = reference.cache
                        AND referenced_signature.store_path_hash = reference.referenced_hash)
)
SELECT selected_content_variant_for_signing.cached_path, selected_content_variant_for_signing.hash,
       selected_content_variant_for_signing.package, selected_content_variant_for_signing.nar_hash,
       selected_content_variant_for_signing.nar_size, selected_content_variant_for_signing."references",
       selected_content_variant_for_signing.cache,
       selected_content_variant_for_signing.cached_path_has_signature,
       unstored.hash IS NULL AS all_referenced_paths_are_stored
FROM selected_content_variant_for_signing
LEFT JOIN selected_content_variant_for_signing_with_unstored_reference unstored
  ON unstored.cache = selected_content_variant_for_signing.cache
 AND unstored.hash = selected_content_variant_for_signing.hash
ORDER BY selected_content_variant_for_signing.cache, selected_content_variant_for_signing.hash
"#,
        seed_choice = chosen_variant(
            "claim_target.hash",
            "claim_target.project",
            "claim_target.cache",
            eligible,
        ),
        reference_choice = chosen_variant(
            "reference.hash",
            "reference.project",
            "reference.cache",
            eligible,
        ),
        rank = variant_rank("cp", "claim.cache"),
    )
}

pub async fn get_project_build_output_cache_ids<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<CacheId>, DbErr> {
    let mut caches: Vec<CacheId> = EProjectCache::find()
        .select_only()
        .column(CProjectCache::Cache)
        .join(
            JoinType::InnerJoin,
            gradient_entity::project_cache::Relation::Cache.def(),
        )
        .filter(CProjectCache::Project.eq(project))
        .filter(CProjectCache::Mode.is_in([
            CacheSubscriptionMode::ReadWrite,
            CacheSubscriptionMode::WriteOnly,
        ]))
        .filter(CCache::Active.eq(true))
        .into_tuple()
        .all(db)
        .await?;
    caches.sort();
    Ok(caches)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, FromQueryResult)]
pub struct AllowedCache {
    pub cache: CacheId,
    pub sign_allowed: bool,
}

pub async fn caches_allowed_to_sign_committed_store_path_content_variant<C: ConnectionTrait>(
    db: &C,
    variant: CachedPathId,
    project: Option<ProjectId>,
    caches: &[CacheId],
    is_project_upload_commit: bool,
) -> Result<Vec<AllowedCache>, DbErr> {
    if caches.is_empty() {
        return Ok(Vec::new());
    }

    let caches: Vec<uuid::Uuid> = caches.iter().map(|c| c.into_inner()).collect();
    AllowedCache::find_by_statement(
        CACHES_ALLOWED_TO_SIGN_COMMITTED_STORE_PATH_CONTENT_VARIANT.bind([
            Value::Uuid(Some(variant.into_inner())),
            Value::Uuid(project.map(|p| p.into_inner())),
            caches.into(),
            Value::Bool(Some(is_project_upload_commit)),
        ]),
    )
    .all(db)
    .await
}

pub async fn get_project_sign_output_cache_ids<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<CacheId>, DbErr> {
    let rows = db
        .query_all_raw(
            CACHES_WHOSE_PROJECT_SUBSCRIPTION_ASKS_FOR_SIGNATURES
                .bind([Value::Uuid(Some(project.into_inner()))]),
        )
        .await?;
    rows.iter()
        .map(|row| Ok(CacheId::new(row.try_get::<uuid::Uuid>("", "cache")?)))
        .collect()
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProvenWorkerCredentialIds {
    pub worker_registrations: Vec<WorkerRegistrationId>,
    pub team_worker: Option<TeamWorkerId>,
}

pub async fn find_worker_credentials_for_project_job<C: ConnectionTrait>(
    db: &C,
    worker_id: &str,
    project: ProjectId,
) -> Result<ProvenWorkerCredentialIds, DbErr> {
    let team_worker = ETeamWorker::find()
        .filter(CTeamWorker::WorkerId.eq(worker_id))
        .filter(CTeamWorker::Active.eq(true))
        .one(db)
        .await?;
    let registrations: Vec<(WorkerRegistrationId, bool)> = EWorkerRegistration::find()
        .select_only()
        .column(CWorkerRegistration::Id)
        .column(CWorkerRegistration::Active)
        .filter(CWorkerRegistration::WorkerId.eq(worker_id))
        .filter(CWorkerRegistration::PeerId.eq(project))
        .into_tuple()
        .all(db)
        .await?;

    Ok(match team_worker {
        Some(_) if !registrations.is_empty() => ProvenWorkerCredentialIds::default(),
        Some(team_worker) => ProvenWorkerCredentialIds {
            worker_registrations: Vec::new(),
            team_worker: Some(team_worker.id),
        },
        None => ProvenWorkerCredentialIds {
            worker_registrations: registrations
                .into_iter()
                .filter_map(|(id, active)| active.then_some(id))
                .collect(),
            team_worker: None,
        },
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use gradient_entity::evaluation::EvaluationStatus;
    use gradient_entity::{
        cache, store_path_content_producer, team, team_worker, worker_registration,
    };
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::ids::{cache_id, project_id, user_id};
    use gradient_fixtures::shape::{Shape, shape};
    use gradient_fixtures::{rows, test_date};
    use gradient_migration::{Migrator, MigratorTrait};
    use gradient_types::ids::{ProjectCacheId, TeamWorkerId, WorkerRegistrationId};
    use sea_orm_migration::SchemaManager;

    async fn insert_owner_and_project(db: &TestDatabase) {
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        crate::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
    }

    fn migration_name_starts_with_store_path_content_variant_trust_prefix(name: &str) -> bool {
        ["m20261221_000001_", "m20261221_000002_"]
            .iter()
            .any(|prefix| name.starts_with(prefix))
    }

    async fn revert_store_path_content_migrations_in_reverse_order(
        conn: &sea_orm::DatabaseConnection,
    ) {
        for migration in Migrator::migrations().into_iter().rev().filter(|m| {
            migration_name_starts_with_store_path_content_variant_trust_prefix(m.name())
        }) {
            migration
                .down(&SchemaManager::new(conn))
                .await
                .expect("the migration reverts");
        }
    }

    async fn reapply_store_path_content_and_index_migrations(conn: &sea_orm::DatabaseConnection) {
        for migration in Migrator::migrations().into_iter().filter(|m| {
            migration_name_starts_with_store_path_content_variant_trust_prefix(m.name())
        }) {
            migration
                .up(&SchemaManager::new(conn))
                .await
                .expect("the migration applies");
        }
    }

    fn project_registration(worker_id: &str) -> worker_registration::Model {
        worker_registration::Model {
            id: WorkerRegistrationId::now_v7(),
            peer_id: project_id(),
            worker_id: worker_id.into(),
            created_by: Some(user_id()),
            ..rows::worker_registration(true, false)
        }
    }

    #[tokio::test]
    async fn rollback_and_reapply_store_path_content_migration_marks_existing_stored_contents_not_server_verified_and_rejects_exact_duplicate_after_allowing_same_nar_hash_with_different_references()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        revert_store_path_content_migrations_in_reverse_order(&conn).await;
        db.execute(
            "INSERT INTO cached_path (id, hash, package, file_hash, file_size, nar_size, nar_hash, created_at) \
             VALUES (uuidv7(), 'existinghash', 'existing', 'sha256:file', 5, 5, 'sha256:nar', now());",
        )
        .await;

        reapply_store_path_content_and_index_migrations(&conn).await;
        db.execute(
            "INSERT INTO cached_path (id, hash, package, file_hash, file_size, nar_size, nar_hash, created_at) \
             VALUES (uuidv7(), 'oldserverhash', 'old-server', 'sha256:file', 5, 5, 'sha256:nar', now());",
        )
        .await;

        assert_eq!(
            db.texts("SELECT nar_object_storage_key FROM cached_path WHERE hash = 'oldserverhash'")
                .await,
            vec!["oldserverhash".to_owned()],
            "The old server binary inserted a cached path without an object key. A trigger set the key to the store hash."
        );
        assert_eq!(
            db.texts(
                "SELECT server_computed_hashes_and_sizes::text || ' ' || nar_object_storage_key \
                 FROM cached_path WHERE hash = 'existinghash'"
            )
            .await,
            vec!["false existinghash".to_owned()],
            "No verification of existing content on the server. The object key for that content is its store hash."
        );
        let verified = |key: &str, references: &str| {
            format!(
                "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, \
                 \"references\", server_computed_hashes_and_sizes, created_at) \
                 VALUES (uuidv7(), 'verifiedhash', '{key}', 'verified', 'sha256:file', 'sha256:nar', 5, \
                 '{references}', true, now())"
            )
        };
        conn.execute_unprepared(&verified("verifiedhash-a", "a-dep"))
            .await
            .expect("the first verified content is stored");
        assert!(
            conn.execute_unprepared(&verified("verifiedhash-b", "b-dep"))
                .await
                .is_ok(),
            "Store a second verified content with other references."
        );
        assert!(
            conn.execute_unprepared(&verified("verifiedhash-c", "a-dep"))
                .await
                .is_err(),
            "Exact duplicates of verified content cause rejection."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn test_previous_release_hash_read_after_down_migration_ignores_new_object_key_and_finds_legacy_row()
     {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("Insert the cache.");
        let (legacy, uploaded) = (CachedPathId::now_v7(), CachedPathId::now_v7());
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{legacy}', 'legacyhash', 'legacyhash', 'legacy', 'sha256:legacy-file', 'sha256:legacy', 1, now()), \
                    ('{uploaded}', 'uploadedhash', 'uploadedhash-filesha', 'uploaded', 'sha256:filesha', 'sha256:uploaded', 1, now()); \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             VALUES (uuidv7(), '{legacy}', '{cache}', 'cache-1:legacy', 0, now()), \
                    (uuidv7(), '{uploaded}', '{cache}', 'cache-1:uploaded', 0, now());",
            cache = cache_id(),
        ))
        .await;

        revert_store_path_content_migrations_in_reverse_order(&conn).await;

        let previous_release_serves = |hash: &str| {
            format!(
                "SELECT cp.hash FROM cached_path cp \
                 JOIN cached_path_signature s ON s.cached_path = cp.id \
                 WHERE cp.hash = '{hash}' AND s.cache = '{cache}' \
                   AND coalesce(s.signature, s.previous_signing_key_signature) IS NOT NULL \
                   AND cp.file_hash IS NOT NULL",
                cache = cache_id(),
            )
        };
        assert_eq!(
            db.texts(&previous_release_serves("uploadedhash")).await,
            Vec::<String>::new(),
            "The previous release can find no path stored under an object key other than its hash."
        );
        assert_eq!(
            db.texts(&previous_release_serves("legacyhash")).await,
            vec!["legacyhash".to_owned()],
            "The previous release can still find the path stored under that hash."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn test_store_path_content_down_migration_resets_build_with_new_object_key_and_keeps_build_with_legacy_cached_path()
     {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let mut builds = Vec::new();
        for (name, hash, status) in [
            (
                "uploaded",
                "uploadedhash",
                gradient_entity::build::BuildStatus::Substituted,
            ),
            (
                "legacy",
                "legacyhash",
                gradient_entity::build::BuildStatus::Completed,
            ),
        ] {
            let derivation = gradient_types::ids::DerivationId::now_v7();
            crate::rows::insert::<EDerivation, _>(&conn, rows::derivation_named(derivation, name))
                .await
                .expect("the derivation is written");
            crate::rows::insert::<EDerivationOutput, _>(
                &conn,
                rows::derivation_output(derivation, hash, Some(1)),
            )
            .await
            .expect("the output is written");
            let build = gradient_types::ids::DerivationBuildId::now_v7();
            crate::rows::insert::<EDerivationBuild, _>(
                &conn,
                MDerivationBuild {
                    id: build,
                    derivation,
                    status,
                    substituted: true,
                    fetchable: true,
                    attempt: 2,
                    created_at: test_date(),
                    updated_at: test_date(),
                    ..Default::default()
                },
            )
            .await
            .expect("the build is written");
            builds.push(build);
        }
        db.execute(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES (uuidv7(), 'uploadedhash', 'uploadedhash-filesha', 'uploaded', 'sha256:filesha', 'sha256:uploaded', 1, now()), \
                    (uuidv7(), 'legacyhash', 'legacyhash', 'legacy', 'sha256:legacy-file', 'sha256:legacy', 1, now()), \
                    (uuidv7(), 'legacyhash', 'legacyhash-filesha', 'legacy', 'sha256:legacy-filesha', 'sha256:legacy-variant', 1, now());",
        )
        .await;

        revert_store_path_content_migrations_in_reverse_order(&conn).await;

        let build_fields = |build: gradient_types::ids::DerivationBuildId| {
            format!(
                "SELECT status || ' ' || substituted || ' ' || attempt || ' ' || fetchable || ' ' || (updated_at > created_at) \
                 FROM derivation_build WHERE id = '{build}'"
            )
        };
        assert_eq!(
            db.texts(&build_fields(builds[0])).await,
            vec!["0 false 0 false true".to_owned()],
            "The down migration deleted the only stored output of this build. The build can now return to the state from the rebuild by the previous release."
        );
        assert_eq!(
            db.texts(&build_fields(builds[1])).await,
            vec!["3 true 2 true false".to_owned()],
            "The output of this build kept a legacy stored row. The down migration left this build unchanged."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn deleting_duplicate_cached_path_signature_rows_keeps_signed_row_when_older_duplicate_is_unsigned()
     {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("Insert the cache.");
        let index_migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20261221_000002_"))
            .expect("the migration is registered");
        index_migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("the migration reverts");
        let (unsigned, signed) = (CachedPathId::now_v7(), CachedPathId::now_v7());
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{unsigned}', 'duplicatehash', 'duplicatehash-unsigned', 'duplicate', 'sha256:unsigned-file', 'sha256:unsigned', 1, now()), \
                    ('{signed}', 'duplicatehash', 'duplicatehash-signed', 'duplicate', 'sha256:signed-file', 'sha256:signed', 1, now()); \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             VALUES (uuidv7(), '{unsigned}', '{cache}', NULL, 0, now() - interval '1 hour'), \
                    (uuidv7(), '{signed}', '{cache}', 'cache-1:signed', 0, now());",
            cache = cache_id(),
        ))
        .await;

        index_migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("the migration applies");

        assert_eq!(
            db.texts("SELECT cached_path::text FROM cached_path_signature")
                .await,
            vec![signed.to_string()],
            "The signed duplicate remains. The unsigned duplicate is older."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn migration_keeps_oldest_duplicate_cached_path_signature_row_and_builds_unique_index() {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        crate::rows::insert::<ECache, _>(&conn, rows::cache())
            .await
            .expect("Insert the cache.");
        let index_migration = Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20261221_000002_"))
            .expect("the migration is registered");
        index_migration
            .down(&SchemaManager::new(&conn))
            .await
            .expect("the migration reverts");
        let (older, newer) = (CachedPathId::now_v7(), CachedPathId::now_v7());
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{older}', 'duplicatehash', 'duplicatehash-older', 'duplicate', 'sha256:older-file', 'sha256:older', 1, now()), \
                    ('{newer}', 'duplicatehash', 'duplicatehash-newer', 'duplicate', 'sha256:newer-file', 'sha256:newer', 1, now()); \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             VALUES (uuidv7(), '{older}', '{cache}', 0, now() - interval '1 hour'), \
                    (uuidv7(), '{newer}', '{cache}', 0, now());",
            cache = cache_id(),
        ))
        .await;

        index_migration
            .up(&SchemaManager::new(&conn))
            .await
            .expect("the migration applies");

        assert_eq!(
            db.texts("SELECT cached_path::text FROM cached_path_signature")
                .await,
            vec![older.to_string()],
            "Expect the oldest signature row for the cache and store path hash to remain."
        );
        assert_eq!(
            db.texts(
                "SELECT indexname::text FROM pg_indexes \
                 WHERE indexname = 'idx-cached_path_signature-cache-store_path_hash'"
            )
            .await,
            vec!["idx-cached_path_signature-cache-store_path_hash".to_owned()],
            "A unique index is present after the migration."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn store_path_content_producer_kind_check_accepts_3_and_rejects_unknown_kinds() {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        let content = CachedPathId::now_v7();
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{content}', 'kindhash', 'kindhash', 'kind', 'sha256:file', 'sha256:nar', 1, now());"
        ))
        .await;
        let store_path_content_producer_insert_sql_for_kind = |kind: i32| {
            format!(
                "INSERT INTO store_path_content_producer (cached_path, kind) VALUES ('{content}', {kind})"
            )
        };

        assert!(
            conn.execute_unprepared(&store_path_content_producer_insert_sql_for_kind(3))
                .await
                .is_ok(),
            "Producer rows of kind 3 don't refer to any source."
        );
        for invalid_producer_kind in [4, -1] {
            assert!(
                conn.execute_unprepared(&store_path_content_producer_insert_sql_for_kind(
                    invalid_producer_kind
                ))
                .await
                .is_err(),
                "The database cannot accept a producer row of kind {invalid_producer_kind}."
            );
        }
        db.finish().await;
    }

    #[tokio::test]
    async fn rollback_and_reapply_store_path_content_migration_accepts_uploaded_by_project_member_producer_row_with_project_and_rejects_duplicate_kind_2_without_project_and_kind_3_with_project()
     {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        revert_store_path_content_migrations_in_reverse_order(&conn).await;
        reapply_store_path_content_and_index_migrations(&conn).await;
        let content = CachedPathId::now_v7();
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{content}', 'memberhash', 'memberhash', 'member', 'sha256:file', 'sha256:nar', 1, now());"
        ))
        .await;
        let producer_insert_sql_for_kind_and_uploading_member_project_id = |kind: i32,
                                                                            project: Option<
            ProjectId,
        >| {
            let project = project.map_or("NULL".to_owned(), |id| format!("'{id}'"));
            format!(
                "INSERT INTO store_path_content_producer (cached_path, kind, uploading_member_project_id) \
                     VALUES ('{content}', {kind}, {project})"
            )
        };
        let member_upload = producer_insert_sql_for_kind_and_uploading_member_project_id(
            kind(StoredContentVariantProducerKind::UploadedByProjectMember),
            Some(project_id()),
        );

        assert!(
            conn.execute_unprepared(&member_upload).await.is_ok(),
            "After I reverted and reapplied the migration, a producer row appeared for an upload by a project member. That row can record the source project."
        );
        assert!(
            conn.execute_unprepared(&member_upload).await.is_err(),
            "A unique key can reject a second row from the same upload producer for the same content and project."
        );
        for (kind, project_set) in [(2, false), (3, true)] {
            let project = project_set.then(project_id);
            assert!(
                conn.execute_unprepared(
                    &producer_insert_sql_for_kind_and_uploading_member_project_id(kind, project)
                )
                .await
                .is_err(),
                "Check constraints reject a producer row of kind {kind}. The flag {project_set} can tell whether an uploading project is set. Only kind 2 can name the uploading project."
            );
        }
        db.finish().await;
    }

    #[tokio::test]
    async fn sql_project_trust_for_worker_credentials_is_true_for_own_project_and_granting_team_and_false_for_other_project_and_revoked_team_grant()
     {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        let second_project_without_worker_registration_or_team_grant =
            crate::rows::insert::<EProject, _>(
                &conn,
                rows::project_with_id(ProjectId::now_v7(), "second-project"),
            )
            .await
            .expect("the second project is written");
        let main_project_worker_registration_row =
            crate::rows::insert::<worker_registration::Entity, _>(
                &conn,
                project_registration("registered-worker"),
            )
            .await
            .expect("the registration is written");
        let team = crate::rows::insert::<team::Entity, _>(&conn, rows::team())
            .await
            .expect("the team is written");
        let test_team_worker_row = crate::rows::insert::<team_worker::Entity, _>(
            &conn,
            team_worker::Model {
                id: TeamWorkerId::now_v7(),
                team: team.id,
                worker_id: "team-worker".into(),
                token_hash: "hash".into(),
                display_name: "Team".into(),
                active: true,
                created_by: Some(user_id()),
                created_at: test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("the team worker is written");
        conn.execute_unprepared(&format!(
            "INSERT INTO team_project (id, team, project, includes_users, includes_workers, role, created_at) \
             VALUES (uuidv7(), '{team}', '{main}', false, true, NULL, now()), \
                    (uuidv7(), '{team}', '{second}', true, false, '{role}', now())",
            role = crate::rows::insert::<ERole, _>(&conn, rows::view_role())
                .await
                .expect("the role is written")
                .id,
            team = team.id,
            main = project_id(),
            second = second_project_without_worker_registration_or_team_grant.id,
        ))
        .await
        .expect("write the team grant");
        let get_project_ids_trusting_worker_credentials =
            async |worker_registration: String, team_worker: String| {
                db.texts(&format!(
                    "SELECT trusting.id::text FROM project trusting WHERE {} ORDER BY trusting.id",
                    project_trusts_worker_credentials_sql(
                        &worker_registration,
                        &team_worker,
                        "trusting.id"
                    )
                ))
                .await
            };

        assert_eq!(
            get_project_ids_trusting_worker_credentials(
                format!("'{}'::uuid", main_project_worker_registration_row.id),
                "NULL::uuid".into(),
            )
            .await,
            vec![project_id().to_string()],
            "A project can trust only the worker registrations that it made."
        );
        assert_eq!(
            get_project_ids_trusting_worker_credentials(
                "NULL::uuid".into(),
                format!("'{}'::uuid", test_team_worker_row.id),
            )
            .await,
            vec![project_id().to_string()],
            "Projects with a team grant that includes workers trust the workers of that team. Projects with only a user grant do not trust them."
        );
        conn.execute_unprepared(&format!(
            "DELETE FROM team_project WHERE project = '{}'",
            project_id()
        ))
        .await
        .expect("team grant revoked");
        assert!(
            get_project_ids_trusting_worker_credentials(
                "NULL::uuid".into(),
                format!("'{}'::uuid", test_team_worker_row.id),
            )
            .await
            .is_empty(),
            "Team workers get no trust from any project after revocation of the team grant."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn find_worker_credentials_for_project_job_returns_no_credentials_when_worker_id_matches_active_team_worker_and_project_registration()
     {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        let team = crate::rows::insert::<team::Entity, _>(&conn, rows::team())
            .await
            .expect("the team is written");
        crate::rows::insert::<team_worker::Entity, _>(
            &conn,
            team_worker::Model {
                id: TeamWorkerId::now_v7(),
                team: team.id,
                worker_id: "shared-worker".into(),
                token_hash: "hash".into(),
                display_name: "Shared".into(),
                active: true,
                created_by: Some(user_id()),
                created_at: test_date(),
                ..Default::default()
            },
        )
        .await
        .expect("the team worker is written");
        crate::rows::insert::<worker_registration::Entity, _>(
            &conn,
            project_registration("shared-worker"),
        )
        .await
        .expect("the registration is written");

        assert_eq!(
            find_worker_credentials_for_project_job(&conn, "shared-worker", project_id())
                .await
                .expect("the credentials are looked up"),
            ProvenWorkerCredentialIds::default(),
            "Worker IDs serve as both team workers and project registrations. They don't provide credentials."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn project_registration_blocks_server_dialed_team_worker_and_duplicate_team_worker_is_rejected()
     {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        let team = crate::rows::insert::<team::Entity, _>(&conn, rows::team())
            .await
            .expect("the team is written");
        let build_team_worker_row_for_dialed_worker =
            |id: TeamWorkerId, url: Option<String>| team_worker::Model {
                id,
                team: team.id,
                worker_id: "dialed-worker".into(),
                token_hash: "hash".into(),
                url,
                display_name: "Dialed".into(),
                active: true,
                created_by: Some(user_id()),
                created_at: test_date(),
                ..Default::default()
            };
        crate::rows::insert::<team_worker::Entity, _>(
            &conn,
            build_team_worker_row_for_dialed_worker(
                TeamWorkerId::now_v7(),
                Some("grpc://worker.example:50051".into()),
            ),
        )
        .await
        .expect("the team worker is written");
        conn.execute_unprepared(&format!(
            "INSERT INTO team_project (id, team, project, includes_users, includes_workers, created_at) \
             VALUES (uuidv7(), '{}', '{}', false, true, now())",
            team.id,
            project_id()
        ))
        .await
        .expect("The insert wrote the team grant.");
        crate::rows::insert::<worker_registration::Entity, _>(
            &conn,
            worker_registration::Model {
                active: false,
                ..project_registration("dialed-worker")
            },
        )
        .await
        .expect("the registration is written");

        assert_eq!(
            find_worker_credentials_for_project_job(&conn, "dialed-worker", project_id())
                .await
                .expect("the credentials are looked up"),
            ProvenWorkerCredentialIds::default(),
            "Project registrations for a worker ID exclude server-dialed team workers from credentials, even inactive registrations."
        );
        assert!(
            crate::rows::insert::<team_worker::Entity, _>(
                &conn,
                build_team_worker_row_for_dialed_worker(TeamWorkerId::now_v7(), None),
            )
            .await
            .is_err(),
            "Both directions of a connection share one worker ID for a single team worker."
        );
        db.finish().await;
    }

    enum CachedPathSignatureRowForTestedContentVariant {
        SignedRowForTestedContentVariant,
        UnsignedPlaceholderRowOnly,
        SignedRowExistsForDifferentContentVariant,
    }

    async fn main_test_project_trusts_content_variant(
        db: &TestDatabase,
        variant: CachedPathId,
    ) -> bool {
        !db.texts(&format!(
            "SELECT 'trusted' WHERE {}",
            project_trusts_content_variant_exists_sql(
                &format!("'{variant}'::uuid"),
                &format!("'{}'::uuid", project_id()),
            )
        ))
        .await
        .is_empty()
    }

    async fn insert_test_cache_with_active_flag_and_project_subscription(
        db: &TestDatabase,
        mode: CacheSubscriptionMode,
        active: bool,
    ) {
        let conn = db.connection();
        crate::rows::insert::<ECache, _>(
            &conn,
            cache::Model {
                active,
                ..rows::cache()
            },
        )
        .await
        .expect("Insert the cache.");
        crate::rows::insert::<EProjectCache, _>(
            &conn,
            rows::project_cache_link(ProjectCacheId::now_v7(), project_id(), cache_id(), mode),
        )
        .await
        .expect("Write a cache subscription.");
    }

    async fn create_test_database_and_check_project_trusts_content_variant_with_cache_subscription_mode_and_active_flag(
        mode: CacheSubscriptionMode,
        active: bool,
        row: CachedPathSignatureRowForTestedContentVariant,
    ) -> bool {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        let second_project_that_registered_worker_that_built_content_variant =
            crate::rows::insert::<EProject, _>(
                &conn,
                rows::project_with_id(ProjectId::now_v7(), "producing-project"),
            )
            .await
            .expect("Insert the project that built the content variant.");
        let other_project_worker_registration_row_as_variant_producer =
            crate::rows::insert::<worker_registration::Entity, _>(
                &conn,
                worker_registration::Model {
                    peer_id: second_project_that_registered_worker_that_built_content_variant.id,
                    ..project_registration("producing-worker")
                },
            )
            .await
            .expect("the registration is written");
        insert_test_cache_with_active_flag_and_project_subscription(&db, mode, active).await;
        let content_variant_id_whose_trust_is_checked = CachedPathId::now_v7();
        let other_content_variant_id = CachedPathId::now_v7();
        let (signed_variant, signature) = match row {
            CachedPathSignatureRowForTestedContentVariant::SignedRowForTestedContentVariant => (
                content_variant_id_whose_trust_is_checked,
                "decode(repeat('ab', 64), 'hex')",
            ),
            CachedPathSignatureRowForTestedContentVariant::UnsignedPlaceholderRowOnly => {
                (content_variant_id_whose_trust_is_checked, "NULL")
            }
            CachedPathSignatureRowForTestedContentVariant::SignedRowExistsForDifferentContentVariant => {
                (other_content_variant_id, "decode(repeat('ab', 64), 'hex')")
            }
        };
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{tested}', 'varianthash', 'varianthash-tested', 'variant', 'sha256:tested-file', 'sha256:tested', 1, now()), \
                    ('{other}', 'varianthash', 'varianthash-other', 'variant', 'sha256:other-file', 'sha256:other', 1, now()); \
             INSERT INTO store_path_content_producer (cached_path, kind, worker_registration) \
             VALUES ('{tested}', {worker}, '{registration}'), ('{other}', {worker}, '{registration}'); \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             VALUES (uuidv7(), '{signed_variant}', '{cache}', {signature}, 0, now());",
            tested = content_variant_id_whose_trust_is_checked,
            other = other_content_variant_id,
            worker = kind(StoredContentVariantProducerKind::BuiltInDispatchedBuildJob),
            registration = other_project_worker_registration_row_as_variant_producer.id,
            cache = cache_id(),
        ))
        .await;

        let trusted = main_test_project_trusts_content_variant(
            &db,
            content_variant_id_whose_trust_is_checked,
        )
        .await;
        db.finish().await;
        trusted
    }

    #[tokio::test]
    async fn project_trusts_other_project_content_variant_from_signed_active_cache_row() {
        assert!(
            create_test_database_and_check_project_trusts_content_variant_with_cache_subscription_mode_and_active_flag(
                CacheSubscriptionMode::ReadOnly,
                true,
                CachedPathSignatureRowForTestedContentVariant::SignedRowForTestedContentVariant,
            )
            .await,
            "A project can trust a content variant on a read of an active cache with a signed row for that variant. The worker that built the variant can belong to another project."
        );
    }

    #[tokio::test]
    async fn another_project_build_is_trusted_with_unsigned_placeholder_signature_in_read_cache() {
        assert!(
            create_test_database_and_check_project_trusts_content_variant_with_cache_subscription_mode_and_active_flag(
                CacheSubscriptionMode::ReadWrite,
                true,
                CachedPathSignatureRowForTestedContentVariant::UnsignedPlaceholderRowOnly,
            )
            .await,
            "The project can use a copy from another project's worker if cache rows the project reads include an unsigned row for that copy."
        );
    }

    async fn setup_previous_release_database_and_settle_parent_build(
        mode: CacheSubscriptionMode,
        sign_outputs: bool,
        cache_has_private_signing_key: bool,
    ) -> (gradient_entity::build::BuildStatus, i32) {
        let cache = CacheId::now_v7();
        let dependency = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let builder = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .evaluation(EvaluationId::now_v7())
            .derivation(&parent)
            .walked()
            .depends_on(&dependency)
            .job()
            .entry_point("parent")
            .probed()
            .shared_build_option(|b| b.wanted = true)
            .into_cache(cache);
        let builder = if cache_has_private_signing_key {
            builder.keyed("signing-key")
        } else {
            builder
        };
        let db = builder.into_database().await;
        let conn = db.connection();
        revert_store_path_content_migrations_in_reverse_order(&conn).await;
        let copy = CachedPathId::now_v7();
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, package, file_hash, file_size, nar_size, nar_hash, created_at) \
             VALUES ('{copy}', '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}', 'dependency', 'sha256:file', 5, 5, 'sha256:nar', now()), \
                    (uuidv7(), '{parent_hash}', 'parent.drv', 'sha256:drv-file', 5, 5, 'sha256:drv', now()); \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             VALUES (uuidv7(), '{copy}', '{cache}', 0, now());",
            parent_hash = "q".repeat(32),
        ))
        .await;
        reapply_store_path_content_and_index_migrations(&conn).await;
        db.execute(&format!(
            "UPDATE project_cache SET mode = {mode}, sign_outputs = {sign_outputs} WHERE cache = '{cache}'",
            mode = super::mode(mode),
        ))
        .await;
        let parent_id = db.id_of(&parent).await;
        crate::graph::can_start::recount_blocking_deps_for_derivation_ids(&conn, &[parent_id])
            .await
            .expect("The recount ran.");
        crate::graph::can_start::settle_created(&conn, &[parent_id])
            .await
            .expect("The parent build was settled.");
        let settled = db.shared_build(&parent).await;
        db.finish().await;
        (settled.status, settled.blocking_deps)
    }

    #[tokio::test]
    async fn new_build_with_read_write_subscription_and_signing_off_is_queued_with_zero_blocking_dependencies_after_upgrade_when_copy_has_only_unsigned_pre_upgrade_cache_row()
     {
        assert_eq!(
            setup_previous_release_database_and_settle_parent_build(
                CacheSubscriptionMode::ReadWrite,
                false,
                true,
            )
            .await,
            (gradient_entity::build::BuildStatus::Queued, 0),
            "The cache that projects read can still hold the pre-upgrade copy, even with an unsigned row. The build for that project can use the copy and reach Queued without a wait on the dependency."
        );
    }

    #[tokio::test]
    async fn test_read_write_build_depends_on_unsigned_pre_upgrade_copy_and_is_queued_with_zero_blocking_dependencies_after_upgrade_migration()
     {
        assert_eq!(
            setup_previous_release_database_and_settle_parent_build(
                CacheSubscriptionMode::ReadWrite,
                true,
                false,
            )
            .await,
            (gradient_entity::build::BuildStatus::Queued, 0),
            "The cache that projects read can still hold the pre-upgrade copy, even with an unsigned row. The build for that project can use the copy and reach Queued without a wait on the dependency."
        );
    }

    #[tokio::test]
    async fn build_after_upgrade_migration_with_read_only_cache_subscription_queues_with_no_blocking_dependencies_when_dependency_is_only_pre_upgrade_unsigned_row()
     {
        assert_eq!(
            setup_previous_release_database_and_settle_parent_build(
                CacheSubscriptionMode::ReadOnly,
                false,
                true,
            )
            .await,
            (gradient_entity::build::BuildStatus::Queued, 0),
            "The cache that projects read can still hold the pre-upgrade copy, even with an unsigned row. The build for that project can use the copy and reach Queued without a wait on the dependency."
        );
    }

    #[tokio::test]
    async fn upgrade_migration_queues_project_build_with_write_only_cache_subscription_and_no_blocking_dependency_when_dependency_is_only_pre_upgrade_unsigned_row()
     {
        assert_eq!(
            setup_previous_release_database_and_settle_parent_build(
                CacheSubscriptionMode::WriteOnly,
                false,
                true,
            )
            .await,
            (gradient_entity::build::BuildStatus::Queued, 0),
            "Write-only subscriptions give projects no cache read access. Builds still use the stored copy of the dependency. Builds still reach Queued without waiting."
        );
    }

    #[tokio::test]
    async fn variant_from_other_project_worker_is_not_trusted_when_cache_subscription_has_signed_row_only_in_write_only_mode()
     {
        assert!(
            !create_test_database_and_check_project_trusts_content_variant_with_cache_subscription_mode_and_active_flag(
                CacheSubscriptionMode::WriteOnly,
                true,
                CachedPathSignatureRowForTestedContentVariant::SignedRowForTestedContentVariant,
            )
            .await,
            "WriteOnly subscriptions to a cache don't grant trust."
        );
    }

    #[tokio::test]
    async fn variant_from_other_project_worker_is_not_trusted_when_read_cache_is_inactive() {
        assert!(
            !create_test_database_and_check_project_trusts_content_variant_with_cache_subscription_mode_and_active_flag(
                CacheSubscriptionMode::ReadOnly,
                false,
                CachedPathSignatureRowForTestedContentVariant::SignedRowForTestedContentVariant,
            )
            .await,
            "Inactive caches provide no trust."
        );
    }

    #[tokio::test]
    async fn other_project_worker_variant_is_not_trusted_when_signed_row_is_for_another_content_variant()
     {
        assert!(
            !create_test_database_and_check_project_trusts_content_variant_with_cache_subscription_mode_and_active_flag(
                CacheSubscriptionMode::ReadOnly,
                true,
                CachedPathSignatureRowForTestedContentVariant::SignedRowExistsForDifferentContentVariant,
            )
            .await,
            "Signed rows for another content variant of the same store path never imply trust in this variant."
        );
    }

    #[tokio::test]
    async fn a_variant_from_a_worker_registered_by_the_project_and_a_variant_uploaded_into_a_cache_the_project_reads_are_trusted_without_a_signature_row()
     {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        let conn = db.connection();
        let main_project_worker_registration_row =
            crate::rows::insert::<worker_registration::Entity, _>(
                &conn,
                project_registration("registered-worker"),
            )
            .await
            .expect("the registration is written");
        insert_test_cache_with_active_flag_and_project_subscription(
            &db,
            CacheSubscriptionMode::ReadOnly,
            true,
        )
        .await;
        let own_worker_built_content_variant_id = CachedPathId::now_v7();
        let uploaded_content_variant_id = CachedPathId::now_v7();
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{built}', 'builthash', 'builthash', 'built', 'sha256:built-file', 'sha256:built', 1, now()), \
                    ('{uploaded}', 'uploadedhash', 'uploadedhash', 'uploaded', 'sha256:uploaded-file', 'sha256:uploaded', 1, now()); \
             INSERT INTO store_path_content_producer (cached_path, kind, worker_registration) \
             VALUES ('{built}', {worker}, '{registration}'); \
             INSERT INTO store_path_content_producer (cached_path, kind, cache) \
             VALUES ('{uploaded}', {upload}, '{cache}');",
            built = own_worker_built_content_variant_id,
            uploaded = uploaded_content_variant_id,
            worker = kind(StoredContentVariantProducerKind::BuiltInDispatchedBuildJob),
            upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
            registration = main_project_worker_registration_row.id,
            cache = cache_id(),
        ))
        .await;

        assert!(
            main_test_project_trusts_content_variant(&db, own_worker_built_content_variant_id)
                .await,
            "The project can trust content variants from workers that the project registered. Caches contain no signature row for those variants."
        );
        assert!(
            main_test_project_trusts_content_variant(&db, uploaded_content_variant_id).await,
            "The project can trust content variants that were uploaded to a cache that the project can read, even when no signature row is present."
        );
        db.finish().await;
    }

    pub(crate) const DEPENDENCY_OUTPUT_STORE_PATH_HASH: &str = "55555555555555555555555555555555";
    pub(crate) const DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH: &str =
        "66666666666666666666666666666666";
    const TEMPORARY_STORE_PATH_HASH_FOR_SECOND_VARIANT_BEFORE_RENAME_TO_DEPENDENCY_HASH: &str =
        "77777777777777777777777777777777";

    pub(crate) struct StorePathContentVariantTrustTestFixture {
        pub(crate) db: TestDatabase,
        pub(crate) parent: DerivationId,
        pub(crate) cache: CacheId,
    }

    pub(crate) async fn setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
        waiting_project_has_read_write_cache_with_private_signing_key: bool,
        add_rows_to_test_shape_builder: impl FnOnce(Shape, &str) -> Shape,
    ) -> StorePathContentVariantTrustTestFixture {
        let dependency_builder_project_evaluation_id = EvaluationId::now_v7();
        let evaluation_id_of_project_waiting_on_parent_build_dependency = EvaluationId::now_v7();
        let cache = CacheId::now_v7();
        let dependency_derivation_path = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent_build_derivation_path = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let shape_builder_after_default_rows_before_extra_rows = shape()
            .evaluation(dependency_builder_project_evaluation_id)
            .derivation(&dependency_derivation_path)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
            .references(&[(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH, "reference")])
            .cached_path(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH, "reference")
            .evaluation(evaluation_id_of_project_waiting_on_parent_build_dependency)
            .derivation(&parent_build_derivation_path)
            .depends_on(&dependency_derivation_path)
            .job();
        let shape_builder_after_default_rows_before_extra_rows =
            if waiting_project_has_read_write_cache_with_private_signing_key {
                shape_builder_after_default_rows_before_extra_rows
                    .into_cache(cache)
                    .keyed("signing-key")
            } else {
                shape_builder_after_default_rows_before_extra_rows
            };
        let db = add_rows_to_test_shape_builder(
            shape_builder_after_default_rows_before_extra_rows,
            &parent_build_derivation_path,
        )
        .into_database()
        .await;
        StorePathContentVariantTrustTestFixture {
            parent: db.id_of(&parent_build_derivation_path).await,
            cache,
            db,
        }
    }

    async fn run_sql_change_assert_fixture_cache_and_return_server_signature_pairs(
        sql_statement_to_run_on_fixture_before_signing_query: &str,
    ) -> Vec<(String, String)> {
        let signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape,
            )
            .await;
        if !sql_statement_to_run_on_fixture_before_signing_query.is_empty() {
            signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                .db
                .execute(sql_statement_to_run_on_fixture_before_signing_query)
                .await;
        }
        let stored_content_variant_and_sign_cache_for_trust_blocked_dependents =
            load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                &signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.db.connection(),
                &[signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.parent],
            )
            .await
            .expect("Query the variants that the server can sign.");
        assert!(stored_content_variant_and_sign_cache_for_trust_blocked_dependents.iter().all(|row| row.cache == signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.cache));
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .finish()
            .await;
        let mut local_test_store_path_hash_package_pairs_from_query: Vec<(String, String)> =
            stored_content_variant_and_sign_cache_for_trust_blocked_dependents
                .into_iter()
                .map(|row| (row.hash, row.package))
                .collect();
        local_test_store_path_hash_package_pairs_from_query.sort();
        local_test_store_path_hash_package_pairs_from_query
    }

    fn expected_dependency_output_and_reference_rows() -> Vec<(String, String)> {
        vec![
            (
                DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(),
                "dependency".to_owned(),
            ),
            (
                DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned(),
                "reference".to_owned(),
            ),
        ]
    }

    async fn test_each_sql_change_has_no_server_side_signature_rows(changes: &[String]) {
        for sql_statement_that_removes_server_side_signature_condition in changes {
            assert_eq!(
                run_sql_change_assert_fixture_cache_and_return_server_signature_pairs(
                    sql_statement_that_removes_server_side_signature_condition
                )
                .await,
                Vec::<(String, String)>::new(),
                "Changes to the fixture prevent the server from signing ({sql_statement_that_removes_server_side_signature_condition})"
            );
        }
    }

    fn build_recursive_sql_before_skipping_already_signed_and_claimed_pairs(
        seeds: &str,
        target_subscription: &str,
        eligible: fn(&str, &str) -> String,
    ) -> String {
        format!(
            r#"
WITH RECURSIVE claim_seed(hash, project) AS ({seeds}),
claim_target AS (
    SELECT DISTINCT claim_seed.hash, claim_seed.project, claim_subscription.cache
    FROM claim_seed
    JOIN project_cache claim_subscription ON claim_subscription.project = claim_seed.project
    WHERE {target_subscription}
),
claim(cached_path, project, cache) AS (
    SELECT chosen.id, claim_target.project, claim_target.cache
    FROM claim_target
    CROSS JOIN LATERAL ({seed_choice}) chosen
    UNION
    SELECT chosen.id, reference.project, reference.cache
    FROM (
        SELECT DISTINCT {STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION} AS hash,
               claim.project, claim.cache
        FROM claim
        JOIN cached_path claimed ON claimed.id = claim.cached_path
        CROSS JOIN LATERAL unnest(string_to_array(claimed."references", ' ')) r(token)
        WHERE r.token <> ''
          AND NOT EXISTS (
              SELECT 1 FROM cached_path_signature s
              WHERE s.cached_path = claimed.id AND s.cache = claim.cache
                AND s.signature IS NOT NULL AND s.references_are_claimed)
    ) reference
    CROSS JOIN LATERAL ({reference_choice}) chosen
),
selected_content_variant_for_signing AS MATERIALIZED (
    SELECT DISTINCT ON (claim.cache, cp.hash)
           cp.id AS cached_path, cp.hash, cp.package, cp.nar_hash, cp.nar_size, cp."references",
           claim.cache,
           coalesce(s.signature IS NOT NULL, false) AS cached_path_has_signature
    FROM claim
    JOIN cached_path cp ON cp.id = claim.cached_path
    LEFT JOIN cached_path_signature s ON s.cached_path = cp.id AND s.cache = claim.cache
    WHERE NOT coalesce(s.signature IS NOT NULL AND s.references_are_claimed, false)
    ORDER BY claim.cache, cp.hash, {rank}
),
store_paths_to_claim(cache, hash) AS MATERIALIZED (
    SELECT DISTINCT claim.cache, claimed_copy.hash
    FROM claim JOIN cached_path claimed_copy ON claimed_copy.id = claim.cached_path
),
references_of_selected_content_variant_for_signing(cache, hash, referenced_hash) AS MATERIALIZED (
    SELECT selected_content_variant_for_signing.cache, selected_content_variant_for_signing.hash, {STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION}
    FROM selected_content_variant_for_signing
    CROSS JOIN LATERAL unnest(string_to_array(selected_content_variant_for_signing."references", ' ')) r(token)
    WHERE r.token <> ''
),
selected_content_variant_for_signing_with_unstored_reference(cache, hash) AS MATERIALIZED (
    SELECT DISTINCT reference.cache, reference.hash
    FROM references_of_selected_content_variant_for_signing reference
    WHERE reference.referenced_hash <> reference.hash
      AND NOT EXISTS (SELECT 1 FROM store_paths_to_claim claimed
                      WHERE claimed.cache = reference.cache AND claimed.hash = reference.referenced_hash)
      AND NOT EXISTS (SELECT 1 FROM cached_path_signature referenced_signature
                      WHERE referenced_signature.cache = reference.cache
                        AND referenced_signature.store_path_hash = reference.referenced_hash)
)
SELECT selected_content_variant_for_signing.cached_path, selected_content_variant_for_signing.hash,
       selected_content_variant_for_signing.package, selected_content_variant_for_signing.nar_hash,
       selected_content_variant_for_signing.nar_size, selected_content_variant_for_signing."references",
       selected_content_variant_for_signing.cache,
       selected_content_variant_for_signing.cached_path_has_signature,
       unstored.hash IS NULL AS all_referenced_paths_are_stored
FROM selected_content_variant_for_signing
LEFT JOIN selected_content_variant_for_signing_with_unstored_reference unstored
  ON unstored.cache = selected_content_variant_for_signing.cache
 AND unstored.hash = selected_content_variant_for_signing.hash
ORDER BY selected_content_variant_for_signing.cache, selected_content_variant_for_signing.hash
"#,
            seed_choice = chosen_variant(
                "claim_target.hash",
                "claim_target.project",
                "claim_target.cache",
                eligible,
            ),
            reference_choice = chosen_variant(
                "reference.hash",
                "reference.project",
                "reference.cache",
                eligible,
            ),
            rank = variant_rank("cp", "claim.cache"),
        )
    }

    fn legacy_seed_sql_builder_listing_one_row_per_live_build_job(
        parent_build_filter: &str,
    ) -> String {
        format!(
            "SELECT DISTINCT output_blocked_by_untrusted_variant.hash, project_with_live_build_job.project, parent.derivation AS parent \
             FROM derivation_build parent \
             JOIN derivation_dependency e ON e.derivation = parent.derivation AND e.kind IN (0, 2) \
             JOIN derivation_build dep ON dep.derivation = e.dependency \
             JOIN derivation_output output_blocked_by_untrusted_variant ON output_blocked_by_untrusted_variant.derivation = e.dependency \
              AND output_blocked_by_untrusted_variant.hash IS NOT NULL \
              AND (e.dependency_outputs IS NULL OR output_blocked_by_untrusted_variant.name = ANY(e.dependency_outputs)) \
             CROSS JOIN LATERAL (SELECT live_task.project {live_jobs}) project_with_live_build_job \
             WHERE {parent_build_filter} AND dep.fetchable AND NOT {unneeded} \
               AND EXISTS (SELECT 1 FROM project_cache unsigned_signing_project_cache_for_waiting_project \
                           WHERE unsigned_signing_project_cache_for_waiting_project.project = project_with_live_build_job.project \
                             AND {usable_signing_target} \
                             AND NOT EXISTS (SELECT 1 FROM cached_path_signature dependency_output_already_published_in_target_cache \
                                             WHERE dependency_output_already_published_in_target_cache.cache = unsigned_signing_project_cache_for_waiting_project.cache \
                                               AND dependency_output_already_published_in_target_cache.store_path_hash = output_blocked_by_untrusted_variant.hash \
                                               AND dependency_output_already_published_in_target_cache.signature IS NOT NULL) \
                             AND EXISTS ({publishable_copy}))",
            live_jobs = crate::graph::predicates::not_aborted_build_jobs_with_unfinished_or_recently_finished_evaluation_sql_for_derivation_build("parent"),
            unneeded = crate::graph::predicates::builds_with_unneeded_output_predicate("e", "dep"),
            usable_signing_target =
                project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key(
                    "unsigned_signing_project_cache_for_waiting_project"
                ),
            publishable_copy = chosen_variant(
                "output_blocked_by_untrusted_variant.hash",
                "project_with_live_build_job.project",
                "unsigned_signing_project_cache_for_waiting_project.cache",
                sql_prefix_for_project_readable_store_path_content_variant_with_nar_hash_and_nar_size,
            ),
        )
    }

    #[derive(FromQueryResult)]
    struct RowTextQueryResult {
        row: String,
    }

    async fn run_query_returning_text_column_in_rolled_back_transaction(
        db: &TestDatabase,
        sql: String,
        values: Vec<Value>,
    ) -> Vec<String> {
        use sea_orm::TransactionTrait;
        let connection = db.connection();
        let transaction = connection.begin().await.expect("Start a test transaction.");
        let rows = RowTextQueryResult::find_by_statement(
            SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING
                .bind_built(sql, values),
        )
        .all(&transaction)
        .await
        .expect("Run the statement against the test database.");
        transaction
            .rollback()
            .await
            .expect("Roll back the test transaction.");
        rows.into_iter().map(|row| row.row).collect()
    }

    fn wrap_insert_statement_to_return_cached_path_signature_row_text(insert: &str) -> String {
        format!(
            "WITH inserted AS ({insert} RETURNING cache, cached_path, store_path_hash, \
             signature IS NULL AS unsigned, references_are_claimed) \
             SELECT concat_ws('|', cache, cached_path, store_path_hash, unsigned, references_are_claimed) AS row \
             FROM inserted ORDER BY 1"
        )
    }

    fn wrap_signing_select_to_return_one_text_per_row(select: &str) -> String {
        format!(
            "SELECT concat_ws('|', signed.cached_path, signed.hash, signed.cache, \
             signed.cached_path_has_signature, signed.all_referenced_paths_are_stored) AS row \
             FROM ({select}) signed ORDER BY 1"
        )
    }

    fn replace_new_fragment_with_old_fragment_once(text: &str, new: &str, old: &str) -> String {
        assert_eq!(
            text.matches(new).count(),
            1,
            "A single occurrence of the new fragment in the current statement text is enough to reconstruct the old statement text."
        );
        text.replacen(new, old, 1)
    }

    #[tokio::test]
    async fn test_signing_query_returns_same_rows_with_old_and_skip_claimed_references_builders() {
        const THIRD_REFERENCED_STORED_PATH_HASH: &str = "88888888888888888888888888888888";
        let make_cached_path_signature_insert_sql_for_stored_copy =
            |hash: &str, package: &str, signature: &str, claimed: bool| {
                format!(
                    "INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at, signature, references_are_claimed) \
                     SELECT uuidv7(), cp.id, (SELECT id FROM cache), 0, now(), {signature}, {claimed} \
                     FROM cached_path cp WHERE cp.hash = '{hash}' AND cp.package = '{package}';"
                )
            };
        let second_cache_id_for_output_signing_with_private_key = CacheId::now_v7();
        let state_label_and_setup_sql_pairs = [
            (
                "dependency output signed with references claimed, its reference unsigned",
                false,
                make_cached_path_signature_insert_sql_for_stored_copy(
                    DEPENDENCY_OUTPUT_STORE_PATH_HASH,
                    "dependency",
                    "'signature'",
                    true,
                ),
            ),
            (
                "Dependency outputs carry signatures. The `references_are_claimed` column is false.",
                false,
                make_cached_path_signature_insert_sql_for_stored_copy(
                    DEPENDENCY_OUTPUT_STORE_PATH_HASH,
                    "dependency",
                    "'signature'",
                    false,
                ),
            ),
            (
                "Output of a dependency that has unsigned placeholder rows",
                false,
                make_cached_path_signature_insert_sql_for_stored_copy(
                    DEPENDENCY_OUTPUT_STORE_PATH_HASH,
                    "dependency",
                    "NULL",
                    false,
                ),
            ),
            (
                "Another variant of the dependency output is in the cache. The claimed references provide a signature for that variant.",
                false,
                format!(
                    "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
                     VALUES (uuidv7(), '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}', 'other-variant', 'other-variant', 'sha256:other', 'sha256:other', 5, now()); {}",
                    make_cached_path_signature_insert_sql_for_stored_copy(
                        DEPENDENCY_OUTPUT_STORE_PATH_HASH,
                        "other-variant",
                        "'signature'",
                        true,
                    ),
                ),
            ),
            (
                "A reference chain where intermediate paths are signed and claim references, with no signature on the final path.",
                false,
                format!(
                    "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
                     VALUES (uuidv7(), '{THIRD_REFERENCED_STORED_PATH_HASH}', 'third', 'third', 'sha256:third', 'sha256:third', 5, now()); \
                     UPDATE cached_path SET \"references\" = '/nix/store/{THIRD_REFERENCED_STORED_PATH_HASH}-third' \
                     WHERE hash = '{DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH}'; {}{}",
                    make_cached_path_signature_insert_sql_for_stored_copy(
                        THIRD_REFERENCED_STORED_PATH_HASH,
                        "third",
                        "NULL",
                        false,
                    ),
                    make_cached_path_signature_insert_sql_for_stored_copy(
                        DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
                        "reference",
                        "'signature'",
                        true,
                    ),
                ),
            ),
            (
                "Two signing caches: only the second cache can sign the dependency output with references claimed. No row for it in the first cache",
                true,
                format!(
                    "INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at, signature, references_are_claimed) \
                     SELECT uuidv7(), cp.id, '{second_cache_id_for_output_signing_with_private_key}', 0, now(), 'signature', true \
                     FROM cached_path cp WHERE cp.hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}' AND cp.package = 'dependency';"
                ),
            ),
        ];
        let project_build_job_output_hashes_insert_sql_seed = "SELECT DISTINCT dout.hash, t.project \
             FROM task t \
             JOIN evaluation e           ON e.task = t.id \
             JOIN build_job bj           ON bj.evaluation = e.id \
             JOIN derivation_output dout ON dout.derivation = bj.derivation \
             WHERE t.project = $1 AND dout.hash IS NOT NULL";
        let pushed_store_path_hashes_insert_sql_seed_copy =
            "SELECT DISTINCT pushed.hash, $1::uuid FROM unnest($2::text[]) pushed(hash)";
        let dependency_derivation_path = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        for (
            fixture_state_label,
            fixture_links_waiting_project_to_second_read_write_signing_cache,
            sql_building_one_fixture_state,
        ) in state_label_and_setup_sql_pairs
        {
            let fixture =
                setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                    true,
                    |shape, _| {
                        let shape = shape
                            .derivation(&dependency_derivation_path)
                            .job()
                            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
                            .mark_uploaded_by_user()
                            .cached_path(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH, "reference")
                            .mark_uploaded_by_user();
                        if fixture_links_waiting_project_to_second_read_write_signing_cache {
                            shape
                                .into_cache(second_cache_id_for_output_signing_with_private_key)
                                .keyed("second-signing-key")
                        } else {
                            shape
                        }
                    },
                )
                .await;
            fixture.db.execute(&sql_building_one_fixture_state).await;
            let waiting_project_id = fixture
                .db
                .rows::<gradient_entity::project_cache::Entity>()
                .await
                .into_iter()
                .find(|subscription| subscription.cache == fixture.cache)
                .map(|subscription| subscription.project.into_inner())
                .expect("This fixture links the signing cache to the waiting project.");
            let three_compared_statement_text_and_binding_replacements = [
                (
                    INSERT_PLACEHOLDERS_FOR_PROJECT_OUTPUTS_INTO_CACHE.text().into_owned(),
                    project_build_job_output_hashes_insert_sql_seed.to_owned(),
                    vec![
                        Value::Uuid(Some(waiting_project_id)),
                        Value::Uuid(Some(fixture.cache.into_inner())),
                    ],
                ),
                (
                    INSERT_PLACEHOLDERS_FOR_PROJECT_STORE_HASHES.text().into_owned(),
                    pushed_store_path_hashes_insert_sql_seed_copy.to_owned(),
                    vec![
                        Value::Uuid(Some(waiting_project_id)),
                        vec![
                            DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(),
                            DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned(),
                        ]
                        .into(),
                    ],
                ),
            ]
            .into_iter()
            .map(|(insert, seeds, values)| {
                (
                    wrap_insert_statement_to_return_cached_path_signature_row_text(
                        &replace_new_fragment_with_old_fragment_once(
                            &insert,
                            &build_sql_for_content_variants_needing_signature_from_store_hash_project_seeds(&seeds),
                            &build_recursive_sql_before_skipping_already_signed_and_claimed_pairs(
                                &seeds,
                                &signing_subscription("claim_subscription"),
                                candidate_content_variant_is_in_target_cache_or_trusted_by_project,
                            ),
                        ),
                    ),
                    wrap_insert_statement_to_return_cached_path_signature_row_text(&insert),
                    values,
                )
            })
            .chain(std::iter::once({
                let seeds = format!(
                    "SELECT DISTINCT blocked.hash, blocked.project FROM ({seeds}) blocked",
                    seeds = seed_missing_signed_dependency_output_cache_rows_for_project_with_live_parent_job_and_signing_cache_subscription(
                        "parent.derivation = ANY($1::uuid[])"
                    ),
                );
                let target_subscription =
                    project_cache_row_has_active_read_write_subscription_that_signs_outputs_and_cache_has_private_signing_key(
                        "claim_subscription",
                    );
                let signing_select_statement_text =
                    SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING
                        .text();
                (
                    wrap_signing_select_to_return_one_text_per_row(
                        &replace_new_fragment_with_old_fragment_once(
                            &signing_select_statement_text,
                            &build_recursive_sql_for_content_variant_to_sign_with_reference_closure_per_cache_and_store_path_hash(
                                &seeds,
                                &target_subscription,
                                sql_prefix_for_project_readable_store_path_content_variant_with_nar_hash_and_nar_size,
                            ),
                            &build_recursive_sql_before_skipping_already_signed_and_claimed_pairs(
                                &seeds,
                                &target_subscription,
                                sql_prefix_for_project_readable_store_path_content_variant_with_nar_hash_and_nar_size,
                            ),
                        ),
                    ),
                    wrap_signing_select_to_return_one_text_per_row(&signing_select_statement_text),
                    vec![vec![fixture.parent.into_inner()].into()],
                )
            }));
            let mut old_statement_row_count = 0;
            for (old, new, values) in three_compared_statement_text_and_binding_replacements {
                let old_statement_returned_rows_as_text =
                    run_query_returning_text_column_in_rolled_back_transaction(
                        &fixture.db,
                        old,
                        values.clone(),
                    )
                    .await;
                assert_eq!(
                    old_statement_returned_rows_as_text,
                    run_query_returning_text_column_in_rolled_back_transaction(
                        &fixture.db,
                        new,
                        values
                    )
                    .await,
                    "The old statement and the new statement insert or return the same row in state {fixture_state_label}"
                );
                old_statement_row_count += old_statement_returned_rows_as_text.len();
            }
            assert!(
                old_statement_row_count > 0,
                "Old statement produced at least one row for state: {fixture_state_label}."
            );
            fixture.db.finish().await;
        }
    }

    #[tokio::test]
    async fn distinct_project_seed_builder_returns_same_signing_rows_for_shared_dependency_build() {
        let second_project_signing_cache_id = CacheId::now_v7();
        let fixture =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, parent| {
                    shape
                        .evaluation(EvaluationId::now_v7())
                        .derivation(parent)
                        .job()
                        .into_cache(second_project_signing_cache_id)
                        .keyed("second-signing-key")
                },
            )
            .await;
        fixture
            .db
            .execute(&format!(
                "CREATE TEMP TABLE waiting_project_evaluation_copy_with_new_id AS SELECT e.* FROM evaluation e \
                 JOIN task t ON t.id = e.task JOIN project_cache pc ON pc.project = t.project WHERE pc.cache = '{cache}'; \
                 CREATE TEMP TABLE evaluation_build_job_copies AS SELECT j.* FROM build_job j \
                 JOIN waiting_project_evaluation_copy_with_new_id e ON e.id = j.evaluation; \
                 UPDATE waiting_project_evaluation_copy_with_new_id SET id = uuidv7(), concurrent = true; \
                 UPDATE evaluation_build_job_copies SET id = uuidv7(), \
                 evaluation = (SELECT id FROM waiting_project_evaluation_copy_with_new_id); \
                 INSERT INTO evaluation SELECT * FROM waiting_project_evaluation_copy_with_new_id; \
                 INSERT INTO build_job SELECT * FROM evaluation_build_job_copies;",
                cache = fixture.cache.into_inner()
            ))
            .await;
        let parent_build_filter = "parent.derivation = ANY($1::uuid[])";
        let seed_row_hash_project_parent_as_text = |seeds: &str| {
            format!(
                "SELECT concat_ws('|', seeded.hash, seeded.project, seeded.parent) AS row \
                 FROM ({seeds}) seeded ORDER BY 1"
            )
        };
        let new_seed_sql = seed_missing_signed_dependency_output_cache_rows_for_project_with_live_parent_job_and_signing_cache_subscription(
            parent_build_filter,
        );
        let old_seed_builder_copy_sql_text =
            legacy_seed_sql_builder_listing_one_row_per_live_build_job(parent_build_filter);
        let parent_derivation_ids: Value = vec![fixture.parent.into_inner()].into();
        assert_eq!(
            run_query_returning_text_column_in_rolled_back_transaction(
                &fixture.db,
                seed_row_hash_project_parent_as_text(&old_seed_builder_copy_sql_text),
                vec![parent_derivation_ids.clone()],
            )
            .await,
            run_query_returning_text_column_in_rolled_back_transaction(
                &fixture.db,
                seed_row_hash_project_parent_as_text(&new_seed_sql),
                vec![parent_derivation_ids.clone()],
            )
            .await,
            "Old and new seed queries list the same rows for a shared dependency build with two live jobs from one project and one from another."
        );
        let signing_select_statement_text =
            SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING
                .text();
        let old_statement_returned_rows_as_text =
            run_query_returning_text_column_in_rolled_back_transaction(
                &fixture.db,
                wrap_signing_select_to_return_one_text_per_row(
                    &replace_new_fragment_with_old_fragment_once(
                        &signing_select_statement_text,
                        &new_seed_sql,
                        &old_seed_builder_copy_sql_text,
                    ),
                ),
                vec![parent_derivation_ids.clone()],
            )
            .await;
        assert_eq!(
            old_statement_returned_rows_as_text,
            run_query_returning_text_column_in_rolled_back_transaction(
                &fixture.db,
                wrap_signing_select_to_return_one_text_per_row(&signing_select_statement_text),
                vec![parent_derivation_ids],
            )
            .await,
            "The old signing query and the new signing query return the same rows for this shared dependency build."
        );
        assert!(
            [fixture.cache, second_project_signing_cache_id]
                .iter()
                .all(|cache| old_statement_returned_rows_as_text
                    .iter()
                    .any(|row| row.contains(&cache.into_inner().to_string()))),
            "The old signature queries return rows for the signing cache of each of the two projects."
        );
        fixture.db.finish().await;
    }

    fn insert_other_stored_variant_and_unsigned_placeholder_signature() -> String {
        format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES (uuidv7(), '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}', 'other-variant', 'other-variant', 'sha256:other', 'sha256:other', 5, now()); \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             SELECT uuidv7(), cp.id, (SELECT id FROM cache), 0, now() FROM cached_path cp \
             WHERE cp.nar_object_storage_key = 'other-variant';"
        )
    }

    fn mark_dependency_derivation_as_fixed_output() -> String {
        format!(
            "UPDATE derivation SET is_fixed_output = true WHERE id IN \
             (SELECT derivation FROM derivation_output WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}')"
        )
    }

    #[tokio::test]
    async fn server_signs_other_project_stored_dependency_output_and_reference_into_waiting_project_read_write_cache()
     {
        assert_eq!(
            run_sql_change_assert_fixture_cache_and_return_server_signature_pairs("").await,
            expected_dependency_output_and_reference_rows(),
            "The server signed the outputs of the stored dependencies and their references. The server put both into the ReadWrite cache of the waiting project."
        );
    }

    #[tokio::test]
    async fn server_signs_other_project_stored_fixed_output_dependency_output_and_referenced_store_path_into_waiting_project_read_write_cache()
     {
        assert_eq!(
            run_sql_change_assert_fixture_cache_and_return_server_signature_pairs(
                &mark_dependency_derivation_as_fixed_output()
            )
            .await,
            expected_dependency_output_and_reference_rows(),
            "The server can sign fixed-output downloads from other projects. The server can sign the references of those downloads too. Those downloads and references go into the ReadWrite cache of the waiting project, like all other inputs."
        );
    }

    #[tokio::test]
    async fn test_nothing_signed_into_readonly_writeonly_or_inactive_cache_subscription() {
        test_each_sql_change_has_no_server_side_signature_rows(&[
            format!(
                "UPDATE project_cache SET mode = {}, sign_outputs = false",
                mode(CacheSubscriptionMode::ReadOnly)
            ),
            format!(
                "UPDATE project_cache SET mode = {}",
                mode(CacheSubscriptionMode::WriteOnly)
            ),
            "UPDATE cache SET active = false".to_owned(),
        ])
        .await;
    }

    #[tokio::test]
    async fn test_nothing_signed_when_subscription_does_not_sign_outputs_or_cache_has_no_private_key()
     {
        test_each_sql_change_has_no_server_side_signature_rows(&[
            "UPDATE project_cache SET sign_outputs = false".to_owned(),
            "UPDATE cache SET private_key = ''".to_owned(),
        ])
        .await;
    }

    #[tokio::test]
    async fn no_seed_row_when_parent_job_is_aborted_or_evaluation_finished() {
        let parent_build_jobs_sql_subquery = "SELECT bj.id FROM build_job bj \
                           JOIN derivation_dependency e ON e.derivation = bj.derivation";
        test_each_sql_change_has_no_server_side_signature_rows(&[
            format!("UPDATE build_job SET aborted = true WHERE id IN ({parent_build_jobs_sql_subquery})"),
            format!(
                "UPDATE evaluation SET status = {terminal}, building_started_at = now() \
                 WHERE id IN (SELECT evaluation FROM build_job WHERE id IN ({parent_build_jobs_sql_subquery}))",
                terminal = crate::sql::status::eval_in(&EvaluationStatus::TERMINAL[..1]),
            ),
        ])
        .await;
    }

    #[tokio::test]
    async fn blocked_input_seed_guard_blocks_signing_when_waiting_project_trusts_variant_from_signed_cache_row_and_seed_has_no_row()
     {
        test_each_sql_change_has_no_server_side_signature_rows(&[format!(
            "INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), cp.id, (SELECT id FROM cache), decode(repeat('ab', 64), 'hex'), 0, now() \
             FROM cached_path cp WHERE cp.hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
        )])
        .await;
    }

    #[tokio::test]
    async fn test_nothing_is_signed_when_one_closure_reference_cannot_be_signed() {
        test_each_sql_change_has_no_server_side_signature_rows(&[format!(
            "DELETE FROM cached_path WHERE hash = '{DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH}'"
        )])
        .await;
    }

    const REFERENCE_TOKEN_WITHOUT_STORED_COPY_STORE_PATH_HASH: &str =
        "88888888888888888888888888888888";
    const UNSIGNABLE_REFERENCED_COPY_STORE_PATH_HASH: &str = "99999999999999999999999999999999";
    const DEPENDENCY_OUTPUT_REFERENCE_IN_UNREAD_CACHE_STORE_PATH_HASH: &str =
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn build_sql_for_dependency_output_copy_reference_with_signing_cache_row(
        reference_tokens: &str,
    ) -> String {
        format!(
            "UPDATE cached_path SET \"references\" = '{reference_tokens}' \
             WHERE hash = '{DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH}'; \
             INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), cp.id, (SELECT id FROM cache), decode(repeat('ab', 64), 'hex'), 0, now() \
             FROM cached_path cp WHERE cp.hash = '{DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH}';"
        )
    }

    #[tokio::test]
    async fn already_signed_reference_with_unclaimable_references_does_not_block_dependency_output_signing()
     {
        assert_eq!(
            run_sql_change_assert_fixture_cache_and_return_server_signature_pairs(
                &build_sql_for_dependency_output_copy_reference_with_signing_cache_row(&format!(
                    "{REFERENCE_TOKEN_WITHOUT_STORED_COPY_STORE_PATH_HASH}-unstored"
                ))
            )
            .await,
            expected_dependency_output_and_reference_rows(),
            "Both the dependency output and its reference copy come back, because reference copies with a signature already cover the copies that depend on them even when one of their references can never get a signature."
        );
    }

    #[derive(Debug, PartialEq, Eq, FromQueryResult)]
    struct StorePathHashAndAllReferencedPathsAreStoredRow {
        hash: String,
        all_referenced_paths_are_stored: bool,
    }

    #[tokio::test]
    async fn signing_query_does_not_claim_signed_copy_references_when_reference_cannot_be_signed_due_to_unstored_path()
     {
        let signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape,
            )
            .await;
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .execute(&format!(
                "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, \"references\", created_at) \
                 VALUES (uuidv7(), '{UNSIGNABLE_REFERENCED_COPY_STORE_PATH_HASH}', 'unsignable', 'unsignable', 'sha256:unsignable', 'sha256:unsignable', 5, \
                         '{REFERENCE_TOKEN_WITHOUT_STORED_COPY_STORE_PATH_HASH}-unstored', now()); {}",
                build_sql_for_dependency_output_copy_reference_with_signing_cache_row(&format!(
                    "{UNSIGNABLE_REFERENCED_COPY_STORE_PATH_HASH}-unsignable"
                ))
            ))
            .await;
        let parents = vec![
            signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                .parent
                .into_inner(),
        ];
        let mut sorted_blocked_dependent_signing_rows =
            StorePathHashAndAllReferencedPathsAreStoredRow::find_by_statement(
                SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING
                    .bind([parents.into()]),
            )
            .all(
                &signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                    .db
                    .connection(),
            )
            .await
            .expect("Query the variants that the server can sign.");
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .finish()
            .await;
        sorted_blocked_dependent_signing_rows.sort_by(|a, b| a.hash.cmp(&b.hash));
        assert_eq!(
            sorted_blocked_dependent_signing_rows,
            vec![
                StorePathHashAndAllReferencedPathsAreStoredRow {
                    hash: DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(),
                    all_referenced_paths_are_stored: true,
                },
                StorePathHashAndAllReferencedPathsAreStoredRow {
                    hash: DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned(),
                    all_referenced_paths_are_stored: false,
                },
            ],
            "The result returned the signed reference copy with all_referenced_paths_are_stored = false. The result left out the reference copy of the signed reference copy, because a path in that copy can never receive a signature."
        );
    }

    #[tokio::test]
    async fn test_worker_built_dependency_output_reference_in_unread_cache_results_in_nothing_signed()
     {
        let signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape,
            )
            .await;
        let unsubscribed_cache_id = CacheId::now_v7();
        let cached_reference_id_not_read_by_waiting_project = CachedPathId::now_v7();
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .execute(&format!(
                "INSERT INTO cache (id, name, display_name, description, active, priority, public_key, private_key, public, created_by, created_at, managed, max_storage_gb, pull_through) \
                 SELECT '{unsubscribed_cache_id}', 'unsubscribed-cache', 'unsubscribed-cache', '', true, priority, '', '', false, created_by, now(), false, max_storage_gb, false \
                 FROM cache WHERE id = '{cache}'; \
                 INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
                 VALUES ('{cached_reference_id_not_read_by_waiting_project}', '{DEPENDENCY_OUTPUT_REFERENCE_IN_UNREAD_CACHE_STORE_PATH_HASH}', \
                         'reference-in-unsubscribed-cache', 'reference-in-unsubscribed-cache', 'sha256:unsubscribed', 'sha256:unsubscribed', 5, now()); \
                 INSERT INTO store_path_content_producer (cached_path, kind, cache) \
                 VALUES ('{cached_reference_id_not_read_by_waiting_project}', {upload}, '{unsubscribed_cache_id}'); \
                 UPDATE cached_path SET \"references\" = '{DEPENDENCY_OUTPUT_REFERENCE_IN_UNREAD_CACHE_STORE_PATH_HASH}-reference-in-unsubscribed-cache' \
                 WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}';",
                cache = signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.cache,
                upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
            ))
            .await;
        let parent =
            signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                .parent;
        let stored_content_variant_and_sign_cache_for_trust_blocked_dependents =
            load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                &signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                    .db
                    .connection(),
                &[parent],
            )
            .await
            .expect("Query the variants that the server can sign.");
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .execute(&build_sql_to_write_signed_signature_rows_and_update_unsigned_placeholders_for_stored_copy_and_cache_pairs(&stored_content_variant_and_sign_cache_for_trust_blocked_dependents))
            .await;
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .finish()
            .await;
        let mut sorted_store_path_hash_package_pairs: Vec<(String, String, CacheId)> =
            stored_content_variant_and_sign_cache_for_trust_blocked_dependents
                .into_iter()
                .map(|row| (row.hash, row.package, row.cache))
                .collect();
        sorted_store_path_hash_package_pairs.sort();
        assert_eq!(
            sorted_store_path_hash_package_pairs,
            Vec::new(),
            "The publish pass does not sign anything for the parent build. The reference copy is only an upload in a cache that the waiting project does not read. So the waiting project cannot read the reference copy. The server must not publish the reference copy into the signing cache of the waiting project."
        );
    }

    const WORKER_BUILT_COPY_STORE_PATH_HASH_REFERENCING_UPLOAD_IN_UNREAD_CACHE: &str =
        "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn sql_to_store_upload_only_in_unread_cache_and_reference_it_from_store_path(
        referencing_store_path_hash: &str,
    ) -> String {
        format!(
            "INSERT INTO cache (id, name, display_name, description, active, priority, public_key, private_key, public, created_by, created_at, managed, max_storage_gb, pull_through) \
             SELECT uuidv7(), 'unsubscribed-cache', 'unsubscribed-cache', '', true, priority, '', '', false, created_by, now(), false, max_storage_gb, false \
             FROM cache; \
             INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES (uuidv7(), '{DEPENDENCY_OUTPUT_REFERENCE_IN_UNREAD_CACHE_STORE_PATH_HASH}', \
                     'reference-in-unsubscribed-cache', 'reference-in-unsubscribed-cache', 'sha256:unsubscribed', 'sha256:unsubscribed', 5, now()); \
             INSERT INTO store_path_content_producer (cached_path, kind, cache) \
             SELECT cp.id, {upload}, (SELECT id FROM cache WHERE name = 'unsubscribed-cache') FROM cached_path cp \
             WHERE cp.hash = '{DEPENDENCY_OUTPUT_REFERENCE_IN_UNREAD_CACHE_STORE_PATH_HASH}'; \
             UPDATE cached_path SET \"references\" = '{DEPENDENCY_OUTPUT_REFERENCE_IN_UNREAD_CACHE_STORE_PATH_HASH}-reference-in-unsubscribed-cache' \
             WHERE hash = '{referencing_store_path_hash}';",
            upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
        )
    }

    #[tokio::test]
    async fn publish_pass_signs_nothing_when_reference_of_reference_exists_only_as_upload_in_unread_cache()
     {
        assert_eq!(
            run_sql_change_assert_fixture_cache_and_return_server_signature_pairs(
                &sql_to_store_upload_only_in_unread_cache_and_reference_it_from_store_path(
                    DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH
                )
            )
            .await,
            Vec::<(String, String)>::new(),
            "Nothing has a signature. The reference copy has a pointer to an upload. Only one cache has that upload and the waiting project doesn't read that cache. The dependency output has a pointer to the reference copy. So the dependency output has no signature either."
        );
    }

    #[tokio::test]
    async fn signed_reference_copy_has_unstored_paths_when_worker_built_reference_is_dropped_for_unread_upload()
     {
        let fixture =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape,
            )
            .await;
        fixture
            .db
            .execute(&format!(
                "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
                 VALUES (uuidv7(), '{WORKER_BUILT_COPY_STORE_PATH_HASH_REFERENCING_UPLOAD_IN_UNREAD_CACHE}', 'worker-built', 'worker-built', 'sha256:worker-built', 'sha256:worker-built', 5, now()); \
                 INSERT INTO store_path_content_producer (cached_path, kind) \
                 SELECT cp.id, {worker} FROM cached_path cp \
                 WHERE cp.hash = '{WORKER_BUILT_COPY_STORE_PATH_HASH_REFERENCING_UPLOAD_IN_UNREAD_CACHE}'; {} {}",
                build_sql_for_dependency_output_copy_reference_with_signing_cache_row(&format!(
                    "{WORKER_BUILT_COPY_STORE_PATH_HASH_REFERENCING_UPLOAD_IN_UNREAD_CACHE}-worker-built"
                )),
                sql_to_store_upload_only_in_unread_cache_and_reference_it_from_store_path(
                    WORKER_BUILT_COPY_STORE_PATH_HASH_REFERENCING_UPLOAD_IN_UNREAD_CACHE
                ),
                worker = kind(StoredContentVariantProducerKind::BuiltInDispatchedBuildJob),
            ))
            .await;
        let parents = vec![fixture.parent.into_inner()];
        let mut rows = StorePathHashAndAllReferencedPathsAreStoredRow::find_by_statement(
            SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING
                .bind([parents.into()]),
        )
        .all(&fixture.db.connection())
        .await
        .expect("Query the variants that the server can sign.");
        fixture.db.finish().await;
        rows.sort_by(|a, b| a.hash.cmp(&b.hash));
        assert_eq!(
            rows,
            vec![
                StorePathHashAndAllReferencedPathsAreStoredRow {
                    hash: DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(),
                    all_referenced_paths_are_stored: true,
                },
                StorePathHashAndAllReferencedPathsAreStoredRow {
                    hash: DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned(),
                    all_referenced_paths_are_stored: false,
                },
            ],
            "A signed reference copy came back with all_referenced_paths_are_stored = false. The result omitted the worker-built reference. That copy referred to an upload in a cache outside the read path of the waiting project."
        );
    }

    #[tokio::test]
    async fn unsigned_placeholder_row_of_same_variant_is_signed_not_treated_as_signed() {
        assert_eq!(
            run_sql_change_assert_fixture_cache_and_return_server_signature_pairs(
                &format!(
                    "INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
                     SELECT uuidv7(), cp.id, (SELECT id FROM cache), 0, now() \
                     FROM cached_path cp WHERE cp.hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
                )
            )
            .await,
            expected_dependency_output_and_reference_rows(),
            "Unsigned placeholder rows of the same variant receive a signature."
        );
    }

    #[tokio::test]
    async fn cache_signs_other_stored_variant_placeholder_row_for_path_instead_of_producer_variant()
    {
        assert_eq!(
            run_sql_change_assert_fixture_cache_and_return_server_signature_pairs(&format!(
                "{other_variant} \
                 INSERT INTO store_path_content_producer (cached_path, kind, cache) \
                 SELECT cp.id, {upload}, (SELECT id FROM cache) FROM cached_path cp \
                 WHERE cp.nar_object_storage_key = 'other-variant';",
                other_variant = insert_other_stored_variant_and_unsigned_placeholder_signature(),
                upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
            ))
            .await,
            vec![(
                DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(),
                "other-variant".to_owned()
            )],
            "The cache can hold one variant for each store path. A stored variant under an unsigned placeholder can receive a signature."
        );
    }

    async fn query_parent_builds_page_on_fixture(
        fixture: &StorePathContentVariantTrustTestFixture,
    ) -> Vec<DerivationId> {
        list_parent_build_derivation_ids_with_dependency_outputs_to_sign_page(
            &fixture.db.connection(),
            DerivationId::new(uuid::Uuid::nil()),
            DerivationId::new(uuid::Uuid::max()),
            500,
        )
        .await
        .expect("Select a page of parent builds.")
    }

    fn mark_parent_build_job_evaluation_completed_hours_ago(hours: u32) -> String {
        format!(
            "UPDATE evaluation SET status = {completed}, building_started_at = now(), \
             finished_at = (now() AT TIME ZONE 'UTC') - interval '{hours} hours' \
             WHERE id IN (SELECT bj.evaluation FROM build_job bj \
                          JOIN derivation_dependency e ON e.derivation = bj.derivation)",
            completed = crate::sql::status::eval_in(&[EvaluationStatus::Completed]),
        )
    }

    #[tokio::test]
    async fn parent_build_page_query_selects_completed_or_building_parent_builds_with_running_evaluation_and_unsigned_readable_dependency_output()
     {
        for status in [
            gradient_entity::build::BuildStatus::Completed,
            gradient_entity::build::BuildStatus::Building,
        ] {
            let fixture =
                setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                    true,
                    |shape, _| shape.build_status(status),
                )
                .await;
            let selected = query_parent_builds_page_on_fixture(&fixture).await;
            let parent = fixture.parent;
            fixture.db.finish().await;
            assert_eq!(
                selected,
                vec![parent],
                "The parent build page returns a Completed or Building parent build whose evaluation is still running and whose dependency output is readable but unsigned."
            );
        }
    }

    #[tokio::test]
    async fn test_gradient_publish_pass_does_not_select_aborted_or_failed_permanent_parent_build() {
        for status in [
            gradient_entity::build::BuildStatus::Aborted,
            gradient_entity::build::BuildStatus::FailedPermanent,
        ] {
            let fixture =
                setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                    true,
                    |shape, _| shape.build_status(status),
                )
                .await;
            let selected = query_parent_builds_page_on_fixture(&fixture).await;
            fixture.db.finish().await;
            assert_eq!(
                selected,
                Vec::new(),
                "Parent builds in state Aborted or FailedPermanent don't enter the selection."
            );
        }
    }

    #[tokio::test]
    async fn parent_build_not_selected_when_target_signing_cache_has_signed_dependency_output() {
        let fixture =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape,
            )
            .await;
        fixture
            .db
            .execute(&format!(
                "INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
                 SELECT uuidv7(), cp.id, (SELECT id FROM cache), decode(repeat('ab', 64), 'hex'), 0, now() \
                 FROM cached_path cp WHERE cp.hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
            ))
            .await;
        let selected = query_parent_builds_page_on_fixture(&fixture).await;
        fixture.db.finish().await;
        assert_eq!(
            selected,
            Vec::new(),
            "The selection can skip the parent build when the signature cache for the target can supply a signed copy of the dependency output."
        );
    }

    #[tokio::test]
    async fn page_with_limit_one_after_cursor_returns_next_parent_build_id() {
        let second_parent_path = format!("/nix/store/{}-second-parent.drv", "r".repeat(32));
        let fixture =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| {
                    shape
                        .derivation(&second_parent_path)
                        .depends_on(&format!("/nix/store/{}-dependency.drv", "p".repeat(32)))
                        .job()
                },
            )
            .await;
        let mut parents = [fixture.parent, fixture.db.id_of(&second_parent_path).await];
        parents.sort();
        let page_after_first_parent =
            list_parent_build_derivation_ids_with_dependency_outputs_to_sign_page(
                &fixture.db.connection(),
                parents[0],
                DerivationId::new(uuid::Uuid::max()),
                1,
            )
            .await
            .expect("Select a page of parent builds.");
        fixture.db.finish().await;
        assert_eq!(
            page_after_first_parent,
            vec![parents[1]],
            "One-item pages after the first parent build ID return the next parent build ID."
        );
    }

    #[tokio::test]
    async fn selects_parent_with_evaluation_finished_within_24_hours_and_rejects_one_finished_25_hours_ago_without_seed_row()
     {
        let fixture =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape.build_status(gradient_entity::build::BuildStatus::Completed),
            )
            .await;
        fixture
            .db
            .execute(&mark_parent_build_job_evaluation_completed_hours_ago(1))
            .await;
        let selected_one_hour_after_finish = query_parent_builds_page_on_fixture(&fixture).await;
        fixture
            .db
            .execute(&mark_parent_build_job_evaluation_completed_hours_ago(25))
            .await;
        let selected_25_hours_after_finish = query_parent_builds_page_on_fixture(&fixture).await;
        let signing_rows_25_hours_after_finish =
            load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                &fixture.db.connection(),
                &[fixture.parent],
            )
            .await
            .expect("Query the variants that the server can sign.")
            .len();
        let parent = fixture.parent;
        fixture.db.finish().await;
        assert_eq!(
            (
                selected_one_hour_after_finish,
                selected_25_hours_after_finish,
                signing_rows_25_hours_after_finish
            ),
            (vec![parent], Vec::new(), 0),
            "Selection rules keep a parent build for 24 hours after evaluation completion. After 25 hours those rules exclude that parent build. No signing rows come from the seed."
        );
    }

    #[tokio::test]
    async fn test_publish_passes_sign_different_parent_slices_when_page_limit_is_smaller_than_parent_count()
     {
        let cache = CacheId::now_v7();
        let dependency_hashes = ["7", "8", "9"].map(|digit| digit.repeat(32));
        let dependency_paths =
            ["7", "8", "9"].map(|digit| format!("/nix/store/{}-dependency.drv", digit.repeat(32)));
        let parent_paths =
            ["7", "8", "9"].map(|digit| format!("/nix/store/{}-parent.drv", digit.repeat(32)));
        let mut builder = shape().evaluation(EvaluationId::now_v7());
        for (hash, path) in dependency_hashes.iter().zip(&dependency_paths) {
            builder = builder
                .derivation(path)
                .output(hash)
                .job()
                .shared_build_option(|b| b.fetchable = true)
                .cached_path(hash, "dependency");
        }
        builder = builder.evaluation(EvaluationId::now_v7());
        for (parent, dependency) in parent_paths.iter().zip(&dependency_paths) {
            builder = builder.derivation(parent).depends_on(dependency).job();
        }
        let db = builder
            .into_cache(cache)
            .keyed("signing-key")
            .into_database()
            .await;
        let mut parents = Vec::new();
        for path in &parent_paths {
            parents.push(db.id_of(path).await);
        }
        parents.sort();
        let mut selected_by_pass = Vec::new();
        let mut signed_count_by_pass = Vec::new();
        for after in [
            DerivationId::new(uuid::Uuid::nil()),
            parents[1],
            DerivationId::new(uuid::Uuid::nil()),
        ] {
            let selected = list_parent_build_derivation_ids_with_dependency_outputs_to_sign_page(
                &db.connection(),
                after,
                DerivationId::new(uuid::Uuid::max()),
                2,
            )
            .await
            .expect("Select a page of parent builds.");
            let rows =
                load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                    &db.connection(),
                    &selected,
                )
                .await
                .expect("Query the variants that the server can sign.");
            if !rows.is_empty() {
                db.execute(&build_sql_to_write_signed_signature_rows_and_update_unsigned_placeholders_for_stored_copy_and_cache_pairs(&rows))
                    .await;
            }
            signed_count_by_pass.push(rows.len());
            selected_by_pass.push(selected);
        }
        db.finish().await;
        assert_eq!(
            (selected_by_pass, signed_count_by_pass),
            (
                vec![parents[..2].to_vec(), parents[2..].to_vec(), Vec::new()],
                vec![2, 1, 0]
            ),
            "Two passes with a page limit of 2 sign two different slices of the three parent builds. A third pass over all IDs can return no ID."
        );
    }

    fn build_sql_to_write_signed_signature_rows_and_update_unsigned_placeholders_for_stored_copy_and_cache_pairs(
        stored_content_variant_and_sign_cache_for_trust_blocked_dependents: &[StorePathContentVariantSignatureTargetRow],
    ) -> String {
        stored_content_variant_and_sign_cache_for_trust_blocked_dependents.iter()
            .map(|row| {
                let (cached_path, cache) = (row.cached_path, row.cache);
                format!(
                    "INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
                     SELECT uuidv7(), '{cached_path}', '{cache}', 0, now() WHERE NOT EXISTS \
                     (SELECT 1 FROM cached_path_signature WHERE cached_path = '{cached_path}' AND cache = '{cache}'); \
                     UPDATE cached_path_signature SET signature = decode(repeat('ab', 64), 'hex') \
                     WHERE cached_path = '{cached_path}' AND cache = '{cache}';"
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn blocked_cache_slot_still_signs_dependency_output_and_stored_reference_copy_and_second_pass_signs_nothing()
     {
        let signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape,
            )
            .await;
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .execute(&format!(
                "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
                 VALUES (uuidv7(), '{DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH}', 'reference-held-copy', 'reference-held-copy', 'sha256:held', 'sha256:held', 5, now()); \
                 INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
                 SELECT uuidv7(), cp.id, (SELECT id FROM cache), 0, now() FROM cached_path cp \
                 WHERE cp.nar_object_storage_key = 'reference-held-copy'; \
                 INSERT INTO store_path_content_producer (cached_path, kind, cache) \
                 SELECT cp.id, {upload}, (SELECT id FROM cache) FROM cached_path cp \
                 WHERE cp.nar_object_storage_key = 'reference-held-copy';",
                upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
            ))
            .await;
        let parent =
            signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                .parent;
        let mut signed_store_path_hash_package_pairs_by_signing_pass = Vec::new();
        for _ in 0..2 {
            let stored_content_variant_and_sign_cache_for_trust_blocked_dependents =
                load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                    &signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.db.connection(),
                    &[parent],
                )
                .await
                .expect("Query the variants that the server can sign.");
            signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                .db
                .execute(&build_sql_to_write_signed_signature_rows_and_update_unsigned_placeholders_for_stored_copy_and_cache_pairs(&stored_content_variant_and_sign_cache_for_trust_blocked_dependents))
                .await;
            let mut sorted_store_path_hash_package_pairs: Vec<(String, String)> =
                stored_content_variant_and_sign_cache_for_trust_blocked_dependents
                    .into_iter()
                    .map(|row| (row.hash, row.package))
                    .collect();
            sorted_store_path_hash_package_pairs.sort();
            signed_store_path_hash_package_pairs_by_signing_pass
                .push(sorted_store_path_hash_package_pairs);
        }
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .finish()
            .await;
        assert_eq!(
            signed_store_path_hash_package_pairs_by_signing_pass,
            vec![
                vec![
                    (
                        DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(),
                        "dependency".to_owned()
                    ),
                    (
                        DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned(),
                        "reference-held-copy".to_owned()
                    ),
                ],
                Vec::new(),
            ],
            "The first signing pass can sign the output of the dependency. This pass can also sign another stored copy of the reference for that dependency. That stored copy can already occupy the reference position in the cache. The second signing pass can find nothing to sign."
        );
    }

    #[tokio::test]
    async fn test_publish_picks_best_ranked_stored_variant_when_other_live_parent_trusts_worse_variant()
     {
        let third_project_that_trusts_its_own_variant_and_has_live_job_on_parent_build_evaluation_id =
            EvaluationId::now_v7();
        let signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, parent_build_derivation_path| {
                    shape
                        .evaluation(
                            third_project_that_trusts_its_own_variant_and_has_live_job_on_parent_build_evaluation_id,
                        )
                        .derivation(parent_build_derivation_path)
                        .job()
                        .cached_path(
                            TEMPORARY_STORE_PATH_HASH_FOR_SECOND_VARIANT_BEFORE_RENAME_TO_DEPENDENCY_HASH,
                            "preferred-dependency",
                        )
                },
            )
            .await;
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .execute(&format!(
                "UPDATE cached_path SET hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}' \
                 WHERE hash = '{TEMPORARY_STORE_PATH_HASH_FOR_SECOND_VARIANT_BEFORE_RENAME_TO_DEPENDENCY_HASH}'"
            ))
            .await;
        let stored_content_variant_and_sign_cache_for_trust_blocked_dependents =
            load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                &signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.db.connection(),
                &[signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.parent],
            )
            .await
            .expect("Query the variants that the server can sign.");
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .finish()
            .await;
        assert_eq!(
            stored_content_variant_and_sign_cache_for_trust_blocked_dependents
                .into_iter()
                .map(|row| (row.hash, row.package))
                .collect::<Vec<_>>(),
            expected_dependency_output_and_reference_rows(),
            "The publish pass can sign the best-ranked stored variant for a waiting build to read. Another live project of the parent build can trust a lower-ranked variant at the same time."
        );
    }

    #[tokio::test]
    async fn variant_signed_into_both_read_write_signing_caches() {
        let waiting_project_second_read_write_signing_cache_id = CacheId::now_v7();
        let signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| {
                    shape
                        .into_cache(waiting_project_second_read_write_signing_cache_id)
                        .keyed("second-signing-key")
                },
            )
            .await;
        let stored_content_variant_and_sign_cache_for_trust_blocked_dependents =
            load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                &signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.db.connection(),
                &[signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.parent],
            )
            .await
            .expect("Query the variants that the server can sign.");
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .finish()
            .await;
        let mut returned_and_expected_cache_ids: [Vec<CacheId>; 2] = [
            stored_content_variant_and_sign_cache_for_trust_blocked_dependents
                .into_iter()
                .filter(|row| row.hash == DEPENDENCY_OUTPUT_STORE_PATH_HASH)
                .map(|row| row.cache)
                .collect(),
            vec![
                signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                    .cache,
                waiting_project_second_read_write_signing_cache_id,
            ],
        ];
        returned_and_expected_cache_ids
            .iter_mut()
            .for_each(|ids| ids.sort());
        let [
            sorted_returned_cache_ids,
            expected_sorted_cache_ids_of_waiting_project_read_write_signing_caches,
        ] = returned_and_expected_cache_ids;
        assert_eq!(
            sorted_returned_cache_ids,
            expected_sorted_cache_ids_of_waiting_project_read_write_signing_caches,
            "All caches in the project for ReadWrite signatures retrieve their variant."
        );
    }

    async fn bounded_parent_build_derivation_ids_with_unsigned_dependency_outputs(
        db: &TestDatabase,
        max_parent_builds_per_publish_pass: usize,
    ) -> Vec<DerivationId> {
        db.texts(&format!(
            "SELECT open_work.parent::text FROM ({parents}) open_work \
             ORDER BY open_work.parent LIMIT {max_parent_builds_per_publish_pass}",
            parents = select_parent_build_derivation_ids_with_missing_dependency_output_signature_publish_work(
                "TRUE"
            ),
        ))
        .await
        .into_iter()
        .map(|parent| {
            DerivationId::new(uuid::Uuid::parse_str(&parent).expect(
                "The open work query can return derivation ids as text. Those ids match the UUID format.",
            ))
        })
        .collect()
    }

    async fn run_one_publish_pass_in_test_database_and_return_sorted_signed_store_path_hashes(
        db: &TestDatabase,
        max_parent_builds_per_publish_pass: usize,
    ) -> Vec<String> {
        let parents = bounded_parent_build_derivation_ids_with_unsigned_dependency_outputs(
            db,
            max_parent_builds_per_publish_pass,
        )
        .await;
        let publish_sql_rows_to_sign =
            load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                &db.connection(),
                &parents,
            )
            .await
            .expect("Query the variants that the server can sign.");
        if !publish_sql_rows_to_sign.is_empty() {
            db.execute(&build_sql_to_write_signed_signature_rows_and_update_unsigned_placeholders_for_stored_copy_and_cache_pairs(&publish_sql_rows_to_sign))
                .await;
        }
        let mut signed_store_path_hashes: Vec<String> = publish_sql_rows_to_sign
            .into_iter()
            .map(|row| row.hash)
            .collect();
        signed_store_path_hashes.sort();
        signed_store_path_hashes
    }

    #[tokio::test]
    async fn publish_passes_sign_two_slices_and_third_pass_selects_nothing_when_open_outputs_exceed_bound_with_parent_without_signing_cache()
     {
        let max_parent_builds_per_publish_pass = 2;
        let dependency_output_store_path_hashes: Vec<String> = (1..=4)
            .map(|dependency_number| format!("{dependency_number:0>32}"))
            .collect();
        let mut test_database_shape_builder = shape().evaluation(EvaluationId::now_v7());
        for dependency_number in 1..=4 {
            test_database_shape_builder = test_database_shape_builder
                .derivation(&format!(
                    "/nix/store/{dependency_number:a>32}-dependency.drv"
                ))
                .output(&format!("{dependency_number:0>32}"))
                .job()
                .shared_build_option(|b| b.fetchable = true)
                .cached_path(&format!("{dependency_number:0>32}"), "dependency");
        }
        test_database_shape_builder = test_database_shape_builder
            .evaluation(EvaluationId::now_v7())
            .into_cache(CacheId::now_v7())
            .keyed("signing-key");
        for dependency_number in 1..=4 {
            test_database_shape_builder = test_database_shape_builder
                .derivation(&format!("/nix/store/{dependency_number:b>32}-parent.drv"))
                .depends_on(&format!(
                    "/nix/store/{dependency_number:a>32}-dependency.drv"
                ))
                .job();
        }
        let parent_build_derivation_path_in_project_without_signing_cache =
            format!("/nix/store/{}-parent.drv", "c".repeat(32));
        let db = test_database_shape_builder
            .evaluation(EvaluationId::now_v7())
            .derivation(&parent_build_derivation_path_in_project_without_signing_cache)
            .depends_on(&format!("/nix/store/{:a>32}-dependency.drv", 1))
            .job()
            .into_database()
            .await;
        let first_publish_pass_signed_sorted_store_path_hashes =
            run_one_publish_pass_in_test_database_and_return_sorted_signed_store_path_hashes(
                &db,
                max_parent_builds_per_publish_pass,
            )
            .await;
        let sorted_store_path_hashes_signed_by_second_publish_pass =
            run_one_publish_pass_in_test_database_and_return_sorted_signed_store_path_hashes(
                &db,
                max_parent_builds_per_publish_pass,
            )
            .await;
        let parent_builds_with_open_publish_work_after_two_passes =
            bounded_parent_build_derivation_ids_with_unsigned_dependency_outputs(
                &db,
                max_parent_builds_per_publish_pass,
            )
            .await;
        db.finish().await;
        let mut sorted_signed_store_path_hashes_from_both_publish_passes = [
            first_publish_pass_signed_sorted_store_path_hashes.clone(),
            sorted_store_path_hashes_signed_by_second_publish_pass.clone(),
        ]
        .concat();
        sorted_signed_store_path_hashes_from_both_publish_passes.sort();
        assert_eq!(
            (
                first_publish_pass_signed_sorted_store_path_hashes.len(),
                sorted_store_path_hashes_signed_by_second_publish_pass.len(),
                sorted_signed_store_path_hashes_from_both_publish_passes,
                parent_builds_with_open_publish_work_after_two_passes,
            ),
            (2, 2, dependency_output_store_path_hashes, Vec::new()),
            "Publish passes sign at most 2 parent builds each. Signatures for 2 dependency outputs come from the first pass. The remaining 2 go to the second pass. Together the two passes sign all 4. No parent build with open work remains after that. The test never selects a parent build of a project with no secret key in its signing cache."
        );
    }

    #[tokio::test]
    async fn test_dependency_output_from_unread_private_cache_is_not_signed_into_read_write_cache_without_private_signing_key()
     {
        let dependency_output_copy_private_cache_id = CacheId::now_v7();
        let dependency = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .into_cache(dependency_output_copy_private_cache_id)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
            .mark_uploaded_by_user()
            .evaluation(EvaluationId::now_v7())
            .derivation(&parent)
            .depends_on(&dependency)
            .job()
            .into_cache(CacheId::now_v7())
            .into_database()
            .await;
        let parent = db.id_of(&parent).await;
        let publish_sql_rows_to_sign =
            load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                &db.connection(),
                &[parent],
            )
            .await
            .expect("Query the variants that the server can sign.");
        db.finish().await;
        assert_eq!(
            publish_sql_rows_to_sign,
            Vec::new(),
            "The only stored copy of a dependency's outputs is in a private cache. The waiting project does not read that cache. The waiting project's read-write cache has no secret signing key. The publish pass signs nothing."
        );
    }

    #[tokio::test]
    async fn test_two_publish_passes_never_select_parent_output_stored_only_in_unread_private_cache_and_write_no_signed_or_placeholder_cached_path_signature_row_while_readable_parent_output_is_signed_in_first_pass()
     {
        let unread_private_cache_id = CacheId::now_v7();
        let signing_cache_id = CacheId::now_v7();
        let unreadable_dependency =
            format!("/nix/store/{}-unreadable-dependency.drv", "p".repeat(32));
        let readable_dependency = format!("/nix/store/{}-readable-dependency.drv", "r".repeat(32));
        let unreadable_parent = format!("/nix/store/{}-unreadable-parent.drv", "q".repeat(32));
        let readable_parent = format!("/nix/store/{}-readable-parent.drv", "s".repeat(32));
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&unreadable_dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .into_cache(unread_private_cache_id)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "unreadable-dependency")
            .mark_uploaded_by_user()
            .derivation(&readable_dependency)
            .output(DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .cached_path(
                DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH,
                "readable-dependency",
            )
            .evaluation(EvaluationId::now_v7())
            .into_cache(signing_cache_id)
            .keyed("signing-key")
            .derivation(&unreadable_parent)
            .depends_on(&unreadable_dependency)
            .job()
            .derivation(&readable_parent)
            .depends_on(&readable_dependency)
            .job()
            .into_database()
            .await;
        let readable_parent = db.id_of(&readable_parent).await;
        let mut parents_selected_by_publish_pass = Vec::new();
        let mut signed_store_path_hashes_by_publish_pass = Vec::new();
        for _ in 0..2 {
            parents_selected_by_publish_pass.push(
                bounded_parent_build_derivation_ids_with_unsigned_dependency_outputs(&db, 10).await,
            );
            signed_store_path_hashes_by_publish_pass.push(
                run_one_publish_pass_in_test_database_and_return_sorted_signed_store_path_hashes(
                    &db, 10,
                )
                .await,
            );
        }
        let signature_rows_of_unreadable_output_in_signing_cache = db
            .texts(&format!(
                "SELECT coalesce(encode(signature, 'hex'), 'NULL') FROM cached_path_signature \
                 WHERE store_path_hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}' AND cache = '{signing_cache_id}'"
            ))
            .await;
        db.finish().await;
        assert_eq!(
            (
                parents_selected_by_publish_pass,
                signed_store_path_hashes_by_publish_pass,
                signature_rows_of_unreadable_output_in_signing_cache,
            ),
            (
                vec![vec![readable_parent], Vec::new()],
                vec![
                    vec![DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned()],
                    Vec::new()
                ],
                Vec::<String>::new(),
            ),
            "The two publish passes never select a parent build with a dependency output that was stored only as an upload in a private cache. The waiting project can't read that cache. The passes write no signature row for that output to the signing cache (neither signed nor unsigned). Selection and signing in the first pass apply to a parent build with a dependency output that a worker built."
        );
    }

    #[tokio::test]
    async fn publish_pass_signs_worker_built_copy_into_signing_cache_and_leaves_upload_copy_without_signature_row()
     {
        let unread_private_cache_id = CacheId::now_v7();
        let signing_cache_id = CacheId::now_v7();
        let upload_copy_id = CachedPathId::now_v7();
        let dependency = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "worker-built")
            .into_cache(unread_private_cache_id)
            .evaluation(EvaluationId::now_v7())
            .into_cache(signing_cache_id)
            .keyed("signing-key")
            .derivation(&parent)
            .depends_on(&dependency)
            .job()
            .into_database()
            .await;
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{upload_copy_id}', '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}', 'upload-in-unread-cache', 'upload-in-unread-cache', \
                     'sha256:upload', 'sha256:upload', 5, now() - interval '1 day'); \
             INSERT INTO store_path_content_producer (cached_path, kind, cache) \
             VALUES ('{upload_copy_id}', {upload}, '{unread_private_cache_id}');",
            upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
        ))
        .await;
        let sql_count_signature_rows_of_upload_copy_in_signing_cache = format!(
            "SELECT count(*)::text FROM cached_path_signature \
             WHERE cached_path = '{upload_copy_id}' AND cache = '{signing_cache_id}'"
        );
        let upload_copy_rows_before_publish_pass = db
            .texts(&sql_count_signature_rows_of_upload_copy_in_signing_cache)
            .await;
        run_one_publish_pass_in_test_database_and_return_sorted_signed_store_path_hashes(&db, 10)
            .await;
        let upload_copy_rows_after_publish_pass = db
            .texts(&sql_count_signature_rows_of_upload_copy_in_signing_cache)
            .await;
        let signed_packages_in_signing_cache = db
            .texts(&format!(
                "SELECT cp.package FROM cached_path_signature s JOIN cached_path cp ON cp.id = s.cached_path \
                 WHERE s.cache = '{signing_cache_id}' AND s.signature IS NOT NULL ORDER BY cp.package"
            ))
            .await;
        db.finish().await;
        assert_eq!(
            (
                signed_packages_in_signing_cache,
                upload_copy_rows_before_publish_pass,
                upload_copy_rows_after_publish_pass,
            ),
            (
                vec!["worker-built".to_owned()],
                vec!["0".to_owned()],
                vec!["0".to_owned()],
            ),
            "Publish passes sign the copies that the worker built and put them in the signing cache of the waiting project. Uploaded copies sit in a private cache that the waiting project doesn't read. They get no signature row in the signing cache before or after a publish pass."
        );
    }

    async fn build_database_with_dependency_upload_and_unsigned_signature_row(
        signing_key_is_usable: bool,
    ) -> (TestDatabase, CachedPathId, CacheId, EvaluationId) {
        let private_cache_id = CacheId::now_v7();
        let signing_cache_id = CacheId::now_v7();
        let waiting_evaluation_id = EvaluationId::now_v7();
        let upload_copy_id = CachedPathId::now_v7();
        let dependency = format!("/nix/store/{}-dependency.drv", "p".repeat(32));
        let parent = format!("/nix/store/{}-parent.drv", "q".repeat(32));
        let shape_with_signing_cache = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&dependency)
            .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
            .job()
            .shared_build_option(|b| b.fetchable = true)
            .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "worker-built")
            .into_cache(private_cache_id)
            .evaluation(waiting_evaluation_id)
            .into_cache(signing_cache_id);
        let shape_with_signing_cache = if signing_key_is_usable {
            shape_with_signing_cache.keyed("signing-key")
        } else {
            shape_with_signing_cache
        };
        let db = shape_with_signing_cache
            .derivation(&parent)
            .depends_on(&dependency)
            .job()
            .into_database()
            .await;
        db.execute(&format!(
            "INSERT INTO cached_path (id, hash, nar_object_storage_key, package, file_hash, nar_hash, nar_size, created_at) \
             VALUES ('{upload_copy_id}', '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}', 'upload-in-private-cache', 'upload-in-private-cache', \
                     'sha256:upload', 'sha256:upload', 5, now() - interval '1 day'); \
             INSERT INTO store_path_content_producer (cached_path, kind, cache) \
             VALUES ('{upload_copy_id}', {upload}, '{private_cache_id}'); \
             INSERT INTO cached_path_signature (id, cached_path, cache, fetch_count, created_at) \
             VALUES (uuidv7(), '{upload_copy_id}', '{signing_cache_id}', 0, now());",
            upload = kind(StoredContentVariantProducerKind::UploadedByUserOrApiKey),
        ))
        .await;
        (db, upload_copy_id, signing_cache_id, waiting_evaluation_id)
    }

    #[tokio::test]
    async fn publish_pass_does_not_sign_upload_in_unread_cache_held_only_by_unsigned_placeholder_row_in_signing_cache()
     {
        for signing_key_is_usable in [true, false] {
            let (db, upload_copy_id, signing_cache_id, waiting_evaluation_id) =
                build_database_with_dependency_upload_and_unsigned_signature_row(
                    signing_key_is_usable,
                )
                .await;
            let waiting_project = db.project_of(waiting_evaluation_id).await;
            let chosen_copy = db
                .texts(&format!(
                    "SELECT chosen.id::text FROM ({}) chosen",
                    crate::store_path_content_variant_reader_choice::single_cached_path_sql_for_project_and_store_hash(
                        &format!("'{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"),
                        &format!("'{waiting_project}'::uuid"),
                    )
                ))
                .await;
            let first_pass =
                run_one_publish_pass_in_test_database_and_return_sorted_signed_store_path_hashes(
                    &db, 10,
                )
                .await;
            let slot_rows = db
                .texts(&format!(
                    "SELECT cp.package || ':' || (s.signature IS NOT NULL)::text \
                     FROM cached_path_signature s JOIN cached_path cp ON cp.id = s.cached_path \
                     WHERE s.cache = '{signing_cache_id}' AND cp.hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'"
                ))
                .await;
            let second_pass =
                run_one_publish_pass_in_test_database_and_return_sorted_signed_store_path_hashes(
                    &db, 10,
                )
                .await;
            db.finish().await;
            assert_eq!(
                (chosen_copy, first_pass, slot_rows, second_pass),
                (
                    vec![upload_copy_id.to_string()],
                    Vec::<String>::new(),
                    vec!["upload-in-private-cache:false".to_owned()],
                    Vec::<String>::new(),
                ),
                "Upload copy X is the copy for the waiting project. The waiting project's unsigned row in signing cache S is only a placeholder. X is its content. The upload put X in a cache that the waiting project doesn't read. So the publish pass doesn't sign anything in S, with or without a usable key in S (usable key: {signing_key_is_usable})."
            );
        }
    }

    fn signed_signature_insert_sql_for_dependency_output_and_reference() -> String {
        format!(
            "INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
             SELECT uuidv7(), cp.id, (SELECT id FROM cache), decode(repeat('ab', 64), 'hex'), 0, now() \
             FROM cached_path cp WHERE cp.hash IN ('{DEPENDENCY_OUTPUT_STORE_PATH_HASH}', '{DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH}')"
        )
    }

    #[tokio::test]
    async fn server_signed_dependency_variant_stays_served_when_subscription_is_read_only_write_only_or_not_signing_or_private_key_missing_but_stops_when_cache_is_deactivated_or_signature_row_is_null()
     {
        let signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape,
            )
            .await;
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .execute(&signed_signature_insert_sql_for_dependency_output_and_reference())
            .await;
        let conn =
            signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                .db
                .connection();
        let binary_cache_serves_dependency_output_variant = async || {
            crate::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
                &conn,
                signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id.cache,
                &[DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned()],
            )
            .await
            .expect("Query the variants from the cache.")
            .contains_key(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
        };
        assert!(
            binary_cache_serves_dependency_output_variant().await,
            "The cache provides the variants that the server signed for waiting projects."
        );

        let condition_remove_and_restore_sql_statement_pairs = [
            (
                format!(
                    "UPDATE project_cache SET mode = {}, sign_outputs = false",
                    mode(CacheSubscriptionMode::ReadOnly)
                ),
                format!(
                    "UPDATE project_cache SET mode = {}, sign_outputs = true",
                    mode(CacheSubscriptionMode::ReadWrite)
                ),
            ),
            (
                format!(
                    "UPDATE project_cache SET mode = {}",
                    mode(CacheSubscriptionMode::WriteOnly)
                ),
                format!(
                    "UPDATE project_cache SET mode = {}",
                    mode(CacheSubscriptionMode::ReadWrite)
                ),
            ),
            (
                "UPDATE project_cache SET sign_outputs = false".to_owned(),
                "UPDATE project_cache SET sign_outputs = true".to_owned(),
            ),
            (
                "UPDATE cache SET private_key = ''".to_owned(),
                "UPDATE cache SET private_key = 'signing-key'".to_owned(),
            ),
            (
                "UPDATE cache SET active = false".to_owned(),
                "UPDATE cache SET active = true".to_owned(),
            ),
            (
                "UPDATE cached_path_signature SET signature = NULL, previous_signing_key_signature = NULL"
                    .to_owned(),
                String::new(),
            ),
        ];
        let mut variant_still_served_after_each_condition_removed = Vec::new();
        for (
            trust_condition_removal_statement,
            sql_statement_that_restores_removed_trust_condition,
        ) in condition_remove_and_restore_sql_statement_pairs
        {
            signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                .db
                .execute(&trust_condition_removal_statement)
                .await;
            variant_still_served_after_each_condition_removed
                .push(binary_cache_serves_dependency_output_variant().await);
            if !sql_statement_that_restores_removed_trust_condition.is_empty() {
                signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
                    .db
                    .execute(&sql_statement_that_restores_removed_trust_condition)
                    .await;
            }
        }
        assert_eq!(
            variant_still_served_after_each_condition_removed,
            vec![true, true, true, true, false, false],
            "The cache can still serve the variants that the cache signed when the subscription mode is ReadOnly or WriteOnly, or the subscription doesn't sign the output, or the cache has no signing key. Inactive caches or rows without signatures serve nothing."
        );
        signing_test_database_dependency_output_parent_derivation_and_read_write_cache_id
            .db
            .finish()
            .await;
    }

    async fn build_database_with_stored_dependency_output_built_by_two_workers_in_different_projects_then_delete_both_worker_registrations()
    -> StorePathContentVariantTrustTestFixture {
        let fixture =
            setup_stored_dependency_variant_with_live_parent_build_job_and_read_write_signing_cache(
                true,
                |shape, _| shape.worker_registration("waiting-project-worker"),
            )
            .await;
        let conn = fixture.db.connection();
        crate::rows::insert::<EProject, _>(
            &conn,
            rows::project_with_id(ProjectId::now_v7(), "third-project"),
        )
        .await
        .expect("The test wrote a third project.");
        fixture
            .db
            .execute(&format!(
                "INSERT INTO store_path_content_producer (cached_path, kind, worker_registration) \
                 SELECT cp.id, {worker}, registration.id FROM cached_path cp, worker_registration registration \
                 WHERE cp.hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}' \
                   AND registration.worker_id = 'waiting-project-worker'",
                worker = kind(StoredContentVariantProducerKind::BuiltInDispatchedBuildJob),
            ))
            .await;
        for registration in fixture
            .db
            .texts("SELECT id::text FROM worker_registration ORDER BY id")
            .await
        {
            conn.execute_unprepared(&format!(
                "DELETE FROM worker_registration WHERE id = '{registration}'"
            ))
            .await
            .expect("the registration is deleted");
        }
        fixture
    }

    #[tokio::test]
    async fn test_deleting_both_worker_registrations_keeps_producer_rows_without_worker_reference_and_worker_built_copy_accepted_but_untrusted_and_still_served_by_signing_cache()
     {
        let fixture =
            build_database_with_stored_dependency_output_built_by_two_workers_in_different_projects_then_delete_both_worker_registrations()
                .await;
        assert_eq!(
            fixture
                .db
                .rows::<store_path_content_producer::Entity>()
                .await
                .into_iter()
                .map(|producer| (
                    producer.kind,
                    producer.worker_registration,
                    producer.team_worker
                ))
                .collect::<Vec<_>>(),
            vec![
                (
                    StoredContentVariantProducerKind::BuiltInDispatchedBuildJob,
                    None,
                    None
                );
                3
            ],
            "Deletions of worker registrations leave the producer row intact. They remove only the reference to the worker."
        );
        assert_eq!(
            fixture
                .db
                .texts(&format!(
                    "SELECT cp.hash FROM cached_path cp \
                     JOIN derivation_build parent_build ON parent_build.derivation = '{parent}' \
                     WHERE cp.hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}' AND {usable}",
                    parent = fixture.parent,
                    usable = crate::graph::predicates::stored_copy_in_input_closure_or_trusted_by_every_project_with_live_job_on_shared_build_sql_condition(
                        "cp.id",
                        "parent_build",
                        &format!("cp.hash = ANY(ARRAY['{DEPENDENCY_OUTPUT_STORE_PATH_HASH}'])"),
                    ),
                ))
                .await,
            vec![DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned()],
            "A shared build can still use the stored copy that a worker built, even after the worker registration is removed."
        );
        assert_eq!(
            fixture
                .db
                .texts(&format!(
                    "SELECT project.id::text FROM project WHERE {trusted}",
                    trusted = project_trusts_content_variant_exists_sql(
                        &format!(
                            "(SELECT id FROM cached_path WHERE hash = '{DEPENDENCY_OUTPUT_STORE_PATH_HASH}')"
                        ),
                        "project.id",
                    ),
                ))
                .await,
            Vec::<String>::new(),
            "No project can trust a stored copy after removal of a worker registration. The project that registered the worker can't trust that copy either."
        );
        assert_eq!(
            load_stored_content_variants_and_reference_closure_to_sign_for_publishing_dependency_outputs_into_project_caches(
                &fixture.db.connection(),
                &[fixture.parent],
            )
            .await
            .expect("Query the variants that the server can sign.")
            .into_iter()
            .map(|row| (row.hash, row.cache))
            .collect::<std::collections::BTreeSet<_>>(),
            [
                (DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned(), fixture.cache),
                (
                    DEPENDENCY_OUTPUT_REFERENCE_STORE_PATH_HASH.to_owned(),
                    fixture.cache
                ),
            ]
            .into(),
            "A deleted worker registration can't block the publish pass from signing dependency outputs and their references into the signing cache of the waiting project. All projects can read every copy that a worker built."
        );
        fixture
            .db
            .execute(&signed_signature_insert_sql_for_dependency_output_and_reference())
            .await;
        assert!(
            crate::store_path_content_variant_reader_choice::get_content_variants_served_by_binary_cache_for_store_path_hashes(
                &fixture.db.connection(),
                fixture.cache,
                &[DEPENDENCY_OUTPUT_STORE_PATH_HASH.to_owned()],
            )
            .await
            .expect("Query the variants from the cache.")
            .contains_key(DEPENDENCY_OUTPUT_STORE_PATH_HASH),
            "A binary cache can still serve a stored copy. The cache signed that copy before removal of the worker registration that built the copy."
        );
        fixture.db.finish().await;
    }

    fn sql_text_compares_reference_token_hash_expression_in_join_on_or_exists(
        sql: &str,
        start: usize,
    ) -> bool {
        let before = sql[..start].trim_end();
        let after =
            sql[start + STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION.len()..].trim_start();
        if !(before.ends_with('=')
            || before.ends_with("<>")
            || after.starts_with('=')
            || after.starts_with("<>"))
        {
            return false;
        }
        let mut depth = 0usize;
        let mut clause = Vec::new();
        for (index, byte) in before.bytes().enumerate().rev() {
            match (byte, depth) {
                (b')', _) => depth += 1,
                (b'(', 0) => {
                    if before[..index].trim_end().ends_with("EXISTS") {
                        return true;
                    }
                    break;
                }
                (b'(', _) => depth -= 1,
                (_, 0) => clause.push(byte),
                _ => {}
            }
        }
        clause.reverse();
        String::from_utf8_lossy(&clause)
            .split_whitespace()
            .rfind(|word| matches!(*word, "ON" | "WHERE"))
            == Some("ON")
    }

    #[test]
    fn publish_statement_sql_store_path_hash_computed_from_reference_token_at_most_three_times_without_join_on_or_exists_comparison()
     {
        let sql =
            SQL_SELECT_STORED_CONTENT_VARIANTS_WITH_REFERENCE_CLOSURE_FOR_PROJECT_CACHE_SIGNING
                .text();
        let starts: Vec<usize> = sql
            .match_indices(STORE_PATH_HASH_FROM_REFERENCE_TOKEN_SQL_EXPRESSION)
            .map(|(start, _)| start)
            .collect();
        assert!(
            starts.len() <= 3,
            "There are more than 3 computations of the store path hash from the reference token in the publish statement."
        );
        assert!(
            !starts.iter().any(|&start| {
                sql_text_compares_reference_token_hash_expression_in_join_on_or_exists(&sql, start)
            }),
            "The publish statement does compare the column with the hash of the store path of the reference token inside a JOIN ... ON or EXISTS subquery."
        );
    }

    const TRUSTED_PROJECT_OUTPUT_STORE_PATH_HASH: &str = "99999999999999999999999999999999";

    async fn read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(
        db: &TestDatabase,
    ) -> Vec<String> {
        db.texts(
            "SELECT coalesce(unsigned_signature_backfill_requested_at::text, 'NULL') \
             FROM project_cache ORDER BY id",
        )
        .await
    }

    async fn count_cached_path_signature_rows_in_cache(db: &TestDatabase, cache: CacheId) -> usize {
        db.texts(&format!(
            "SELECT id::text FROM cached_path_signature WHERE cache = '{cache}'"
        ))
        .await
        .len()
    }

    async fn make_test_database_with_trusted_build_output_and_owed_signing_subscription()
    -> (TestDatabase, CacheId, PendingSignatureBackfillSubscription) {
        let cache = CacheId::now_v7();
        let db = shape()
            .evaluation(EvaluationId::now_v7())
            .derivation(&format!("/nix/store/{}-output.drv", "r".repeat(32)))
            .output(TRUSTED_PROJECT_OUTPUT_STORE_PATH_HASH)
            .job()
            .cached_path(TRUSTED_PROJECT_OUTPUT_STORE_PATH_HASH, "output")
            .into_cache(cache)
            .keyed("signing-key")
            .into_database()
            .await;
        let owed =
            read_subscriptions_owing_signature_backfill_after_cursor(&db.connection(), None, 10)
                .await
                .expect("the owed subscriptions are read");
        let [owed] = owed.as_slice() else {
            panic!("{owed:?}");
        };
        let owed = owed.clone();
        (db, cache, owed)
    }

    #[tokio::test]
    async fn test_project_cache_trigger_sets_unsigned_signature_backfill_requested_at_from_sign_outputs_on_insert_and_update()
     {
        let db = TestDatabase::create().await;
        insert_owner_and_project(&db).await;
        insert_test_cache_with_active_flag_and_project_subscription(
            &db,
            CacheSubscriptionMode::ReadOnly,
            true,
        )
        .await;
        let read_only =
            read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await;
        db.execute("DELETE FROM project_cache").await;
        crate::rows::insert::<EProjectCache, _>(
            &db.connection(),
            rows::project_cache_link(
                ProjectCacheId::now_v7(),
                project_id(),
                cache_id(),
                CacheSubscriptionMode::ReadWrite,
            ),
        )
        .await
        .expect("Write a cache subscription.");
        let signing = read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await;
        db.execute(&format!(
            "UPDATE project_cache SET mode = {write_only}, sign_outputs = true",
            write_only = mode(CacheSubscriptionMode::WriteOnly),
        ))
        .await;
        let kept = read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await;
        db.execute("UPDATE project_cache SET sign_outputs = false")
            .await;
        let off = read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await;
        db.execute("UPDATE project_cache SET sign_outputs = true")
            .await;
        let again_is_later = db
            .texts(&format!(
                "SELECT (unsigned_signature_backfill_requested_at > '{}'::timestamp)::text \
                 FROM project_cache",
                signing[0]
            ))
            .await;

        assert_eq!(
            read_only,
            vec!["NULL".to_owned()],
            "A read-only subscription doesn't sign, so there's no owed request."
        );
        assert_ne!(
            signing,
            vec!["NULL".to_owned()],
            "New signature subscriptions record the time of the backfill request."
        );
        assert_eq!(
            kept, signing,
            "Changes that leave signatures on keep the requested time."
        );
        assert_eq!(
            off,
            vec!["NULL".to_owned()],
            "Signature disabled: requested time cleared."
        );
        assert_eq!(
            again_is_later,
            vec!["true".to_owned()],
            "Enabling signing again can record a later request time."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn test_backfill_for_owed_signing_subscription_writes_unsigned_placeholder_rows_to_project_output_cache_and_clears_requested_time()
     {
        let (db, cache, owed) =
            make_test_database_with_trusted_build_output_and_owed_signing_subscription().await;

        let outcome = write_unsigned_signature_placeholders_for_project_earlier_outputs_then_clear_owed_subscription_requested_time_in_locked_transaction_and_commit(
            &db.connection(),
            &owed,
        )
        .await
        .expect("the backfill runs");

        assert_eq!(
            (
                outcome,
                count_cached_path_signature_rows_in_cache(&db, cache).await,
                read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await,
            ),
            (
                BackfillRowsWrittenOrSkippedForLockHeldElsewhere::BackfillWrotePlaceholderRows(1),
                1,
                vec!["NULL".to_owned()],
            ),
            "Backfill can write unsigned placeholders for project outputs to the signature cache. The same step can clear the requested time."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn backfill_keeps_later_requested_time_after_owed_row_read() {
        let (db, _, owed) =
            make_test_database_with_trusted_build_output_and_owed_signing_subscription().await;
        db.execute(
            "UPDATE project_cache SET unsigned_signature_backfill_requested_at = \
             unsigned_signature_backfill_requested_at + interval '1 minute'",
        )
        .await;
        let later = read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await;

        write_unsigned_signature_placeholders_for_project_earlier_outputs_then_clear_owed_subscription_requested_time_in_locked_transaction_and_commit(
            &db.connection(),
            &owed,
        )
        .await
        .expect("the backfill runs");

        assert_eq!(
            read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await,
            later,
            "Requests made during the backfill stay pending with their later time."
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn deleted_subscription_while_owed_backfill_writes_nothing_and_signing_off_clears_requested_time()
     {
        let (deleted, cache, owed) =
            make_test_database_with_trusted_build_output_and_owed_signing_subscription().await;
        deleted.execute("DELETE FROM project_cache").await;
        let outcome = write_unsigned_signature_placeholders_for_project_earlier_outputs_then_clear_owed_subscription_requested_time_in_locked_transaction_and_commit(
            &deleted.connection(),
            &owed,
        )
        .await
        .expect("the backfill runs");
        assert_eq!(
            (
                outcome,
                count_cached_path_signature_rows_in_cache(&deleted, cache).await
            ),
            (
                BackfillRowsWrittenOrSkippedForLockHeldElsewhere::BackfillWrotePlaceholderRows(0),
                0
            ),
            "Subscriptions deleted during a pending backfill receive no placeholder and the backfill doesn't fail."
        );
        deleted.finish().await;

        let (signing_off, _, _) =
            make_test_database_with_trusted_build_output_and_owed_signing_subscription().await;
        signing_off
            .execute("UPDATE project_cache SET sign_outputs = false")
            .await;
        assert_eq!(
            read_subscriptions_owing_signature_backfill_after_cursor(
                &signing_off.connection(),
                None,
                10
            )
            .await
            .expect("the owed subscriptions are read"),
            Vec::new(),
            "Turning signatures off with a backfill pending can clear the requested time. Subscriptions with a pending backfill then don't show in the list."
        );
        signing_off.finish().await;
    }

    #[tokio::test]
    async fn subscription_backfill_skips_while_subscription_lock_is_held_and_writes_after_release()
    {
        use sea_orm::TransactionTrait;
        let (db, cache, owed) =
            make_test_database_with_trusted_build_output_and_owed_signing_subscription().await;
        let requested =
            read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await;
        let connection = db.connection();
        let lock_holder = connection.begin().await.expect("Start a test transaction.");
        lock_holder
            .execute_raw(SUBSCRIPTION_BACKFILL_LOCK_TRY.bind([owed.id.into_inner().into()]))
            .await
            .expect("the second connection takes the lock");

        let while_held = (
            write_unsigned_signature_placeholders_for_project_earlier_outputs_then_clear_owed_subscription_requested_time_in_locked_transaction_and_commit(
                &connection,
                &owed,
            )
            .await
            .expect("the backfill runs"),
            count_cached_path_signature_rows_in_cache(&db, cache).await,
            read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await,
        );
        lock_holder
            .rollback()
            .await
            .expect("Roll back the test transaction.");
        let after_release = (
            write_unsigned_signature_placeholders_for_project_earlier_outputs_then_clear_owed_subscription_requested_time_in_locked_transaction_and_commit(
                &connection,
                &owed,
            )
            .await
            .expect("the backfill runs"),
            count_cached_path_signature_rows_in_cache(&db, cache).await,
            read_all_project_cache_backfill_requested_at_as_text_ordered_by_id(&db).await,
        );

        assert_eq!(
            while_held,
            (
                BackfillRowsWrittenOrSkippedForLockHeldElsewhere::SkippedBecauseAnotherServerHoldsSubscriptionLock,
                0,
                requested,
            ),
            "Backfill jobs write nothing while another connection is the owner of the lock for that subscription. The subscription is unprocessed."
        );
        assert_eq!(
            after_release,
            (
                BackfillRowsWrittenOrSkippedForLockHeldElsewhere::BackfillWrotePlaceholderRows(1),
                1,
                vec!["NULL".to_owned()],
            ),
            "The backfill wrote a placeholder and cleared the requested time after lock release."
        );
        db.finish().await;
    }

    #[test]
    fn placeholder_backfill_and_owed_subscription_list_use_sweep_tier_and_background_reason_not_web_request()
     {
        let backfill = &INSERT_PLACEHOLDERS_FOR_PROJECT_OUTPUTS_INTO_CACHE;
        assert_eq!(
            (
                backfill.tier,
                backfill.budget.buffers,
                backfill.budget.reason.is_some_and(|reason| {
                    reason.contains("signature-backfill") && !reason.contains("/projects/")
                }),
            ),
            (crate::sql::Tier::Sweep, 1_500_000, true),
            "Backfill statements execute on a background path on tier Sweep. Those statements name that path, not a web request. The path name is the reason."
        );
        assert_eq!(
            SELECT_PROJECT_CACHE_ROWS_THAT_OWE_SIGNATURE_BACKFILL_AFTER_CURSOR_OLDEST_FIRST_WITH_LIMIT
                .tier,
            crate::sql::Tier::Sweep,
            "Subscriptions with a pending backfill form a list for tier Sweep to execute."
        );
    }

    #[test]
    fn only_migration_and_trust_module_write_unsigned_signature_backfill_requested_at() {
        let backend = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate lies in the backend directory");
        let unsigned_signature_backfill_requested_at_column_name =
            "unsigned_signature_backfill_requested_at";
        let source_text_writes_project_cache_unsigned_signature_backfill_requested_at =
            |source: &str| {
                let source_text_without_whitespace: String =
                    source.chars().filter(|c| !c.is_whitespace()).collect();
                let whitespace_free_lowercased_source_text =
                    source_text_without_whitespace.to_lowercase();
                let raw_sql_statement_start_has_column_before_column_or_set_list_end =
                    |statement: &str, list_end_markers: &[&str]| {
                        whitespace_free_lowercased_source_text
                            .match_indices(statement)
                            .any(|(start, _)| {
                                let sql_text_through_set_or_insert_column_list =
                                    &whitespace_free_lowercased_source_text
                                        [start + statement.len()..];
                                let set_or_column_list_end_offset = list_end_markers
                                    .iter()
                                    .filter_map(|marker| {
                                        sql_text_through_set_or_insert_column_list.find(marker)
                                    })
                                    .min()
                                    .unwrap_or(sql_text_through_set_or_insert_column_list.len());
                                sql_text_through_set_or_insert_column_list
                                    [..set_or_column_list_end_offset]
                                    .contains(unsigned_signature_backfill_requested_at_column_name)
                            })
                    };
                let column_is_written_through_active_model_field = source_text_without_whitespace
                    .match_indices(unsigned_signature_backfill_requested_at_column_name)
                    .any(|(start, name)| {
                        let text_after_column_name_occurrence =
                            &source_text_without_whitespace[start + name.len()..];
                        let assigned_expression = text_after_column_name_occurrence
                            .get(1..)
                            .unwrap_or_default();
                        let assigned_expression = &assigned_expression[..assigned_expression
                            .find([',', ';', '}'])
                            .unwrap_or(assigned_expression.len())];
                        let is_column_value_member_assignment = text_after_column_name_occurrence
                            .starts_with('=')
                            && !text_after_column_name_occurrence.starts_with("==")
                            && source_text_without_whitespace[..start].ends_with('.');
                        let is_active_value_set_struct_field_initializer =
                            text_after_column_name_occurrence.starts_with(':')
                                && !text_after_column_name_occurrence.starts_with("::")
                                && ["Set(", "set(", "ActiveValue", "active_value"]
                                    .iter()
                                    .any(|setter| assigned_expression.contains(setter));
                        is_column_value_member_assignment
                            || is_active_value_set_struct_field_initializer
                    });
                source_text_without_whitespace
                    .contains("Column::UnsignedSignatureBackfillRequestedAt")
                    || column_is_written_through_active_model_field
                    || raw_sql_statement_start_has_column_before_column_or_set_list_end(
                        "updateproject_cache",
                        &["where", "\"", ";"],
                    )
                    || raw_sql_statement_start_has_column_before_column_or_set_list_end(
                        "insertintoproject_cache",
                        &[")"],
                    )
            };
        let mut directories: Vec<std::path::PathBuf> = std::fs::read_dir(backend)
            .expect("the backend directory reads")
            .filter_map(Result::ok)
            .map(|crate_directory| crate_directory.path().join("src"))
            .chain([backend.join("src")])
            .filter(|source| {
                source.is_dir() && !source.starts_with(backend.join("gradient-migration"))
            })
            .collect();
        let mut writers = Vec::new();
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(&directory)
                .expect("the source directory reads")
                .filter_map(Result::ok)
            {
                let path = entry.path();
                if path.is_dir() {
                    directories.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs")
                    && !path.ends_with(file!())
                {
                    let source = std::fs::read_to_string(&path).unwrap_or_default();
                    if source_text_writes_project_cache_unsigned_signature_backfill_requested_at(
                        &source,
                    ) {
                        writers.push(path);
                    }
                }
            }
        }
        assert_eq!(
            writers,
            Vec::<std::path::PathBuf>::new(),
            "Only database triggers write the column for the request time. The write side of sign_outputs can let the trigger handle the write."
        );
    }
}
