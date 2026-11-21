/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::ids::{CacheId, DerivationBuildId, EvaluationId, ProjectId};

use crate::database::TestDatabase;
use crate::shape::{Shape, shape};

pub const SHARED: &str = "11111111111111111111111111111111";
const SECOND_VARIANT_PLACEHOLDER: &str = "22222222222222222222222222222222";
pub const UNTRUSTED: &str = "33333333333333333333333333333333";
pub const FIRST_KEY: &str = "11111111111111111111111111111111-abc";
pub const SECOND_KEY: &str = "11111111111111111111111111111111-second";
pub const BUILD_A: &str = "/nix/store/44444444444444444444444444444444-a.drv";
pub const BUILD_B: &str = "/nix/store/55555555555555555555555555555555-b.drv";

pub struct Readers {
    pub db: TestDatabase,
    pub project_a: ProjectId,
    pub project_b: ProjectId,
    pub cache_one: CacheId,
    pub cache_two: CacheId,
    pub build_a: DerivationBuildId,
    pub build_b: DerivationBuildId,
}

pub async fn two_variants_of_one_hash_and_one_untrusted_hash() -> Readers {
    create_test_database_with_store_path_content_variants_and_return_readers(|shape, _, _| shape)
        .await
}

pub async fn create_test_database_with_project_a_build_job_on_project_b_derivation_and_optional_upload_cache_read(
    project_a_reads_the_upload_cache: bool,
) -> Readers {
    create_test_database_with_store_path_content_variants_and_return_readers(
        |shape, evaluation_a, upload_cache| {
            let joined = shape.evaluation(evaluation_a).derivation(BUILD_B).job();
            if project_a_reads_the_upload_cache {
                joined.into_cache(upload_cache)
            } else {
                joined
            }
        },
    )
    .await
}

async fn create_test_database_with_store_path_content_variants_and_return_readers(
    extend: impl FnOnce(Shape, EvaluationId, CacheId) -> Shape,
) -> Readers {
    let (evaluation_a, evaluation_b, evaluation_c) = (
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
        EvaluationId::now_v7(),
    );
    let (cache_one, cache_two) = (CacheId::now_v7(), CacheId::now_v7());
    let shared_hash_variants = shape()
        .evaluation(evaluation_a)
        .derivation(BUILD_A)
        .job()
        .cached_path(SHARED, "shared")
        .mark_cached_path_built_by_team_worker()
        .into_cache(cache_one)
        .evaluation(evaluation_b)
        .derivation(BUILD_B)
        .job()
        .cached_path(SECOND_VARIANT_PLACEHOLDER, "shared")
        .into_cache(cache_two)
        .mark_uploaded_by_user()
        .evaluation(evaluation_c)
        .cached_path(UNTRUSTED, "untrusted");
    let db = extend(shared_hash_variants, evaluation_a, cache_two)
        .into_database()
        .await;
    db.execute(&format!(
        "UPDATE cached_path SET hash = '{SHARED}', nar_object_storage_key = '{SECOND_KEY}', \
         nar_hash = 'sha256:second' WHERE hash = '{SECOND_VARIANT_PLACEHOLDER}'; \
         INSERT INTO cached_path_signature (id, cached_path, cache, signature, fetch_count, created_at) \
         SELECT uuidv7(), signed.id, signed.cache, convert_to('signature', 'UTF8'), 0, now() \
         FROM (SELECT id, '{cache_one}'::uuid AS cache FROM cached_path \
               WHERE nar_object_storage_key = '{FIRST_KEY}' OR hash = '{UNTRUSTED}' \
               UNION ALL SELECT id, '{cache_two}'::uuid FROM cached_path \
               WHERE nar_object_storage_key = '{SECOND_KEY}') signed;"
    ))
    .await;
    Readers {
        project_a: db.project_of(evaluation_a).await,
        project_b: db.project_of(evaluation_b).await,
        build_a: db.shared_build(BUILD_A).await.id,
        build_b: db.shared_build(BUILD_B).await.id,
        cache_one,
        cache_two,
        db,
    }
}
