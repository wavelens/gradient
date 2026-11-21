/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use gradient_entity::build::BuildStatus;
use gradient_entity::build_attempt::AttemptOutcome;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::{cache_user, cached_path_signature, project_user, session, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_fixtures::shape::shape;
use gradient_graph::Graph;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer_for_user;
use gradient_types::SessionId;
use gradient_types::consts::{BASE_CACHE_ROLE_VIEW_ID, BASE_ROLE_VIEW_ID};
use gradient_types::ids::*;
use sea_orm::{ActiveModelBehavior, ActiveModelTrait, EntityTrait, IntoActiveModel};
use serde_json::Value;

const DEPENDENCY_DERIVATION_STORE_PATH: &str = "d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1d1-dependency.drv";
const DEPENDENCY_OUTPUT_STORE_PATH_HASH: &str = "d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0d0";
const PARENT_BUILD_DERIVATION_PATH: &str = "a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1a1-parent.drv";
const PARENT_BUILD_OUTPUT_STORE_PATH_HASH: &str = "a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0a0";

struct TestSetupForParentBuildUsingDependencyOutputFromUnreadPrivateCache {
    db: TestDatabase,
    server: TestServer,
    unread_private_cache_name: String,
    building_project_cache_name: String,
    other_project_evaluation: EvaluationId,
    other_project_dependency_build_job: BuildJobId,
    building_project_member_session_token: String,
    other_project_member_session_token: String,
}

async fn insert<M: IntoActiveModel<A>, A: ActiveModelTrait + ActiveModelBehavior + Send>(
    db: &TestDatabase,
    row: M,
) -> <A::Entity as EntityTrait>::Model
where
    <A::Entity as EntityTrait>::Model: IntoActiveModel<A>,
{
    row.into_active_model()
        .insert(&db.connection())
        .await
        .expect("the row is written")
}

async fn insert_user_and_login_session_return_user_id_and_session_token(
    db: &TestDatabase,
    username: &str,
) -> (UserId, String) {
    let user = insert(
        db,
        user::Model {
            id: UserId::now_v7(),
            username: username.into(),
            name: username.into(),
            email: format!("{username}@example.com"),
            ..rows::user()
        },
    )
    .await;
    let session_id = SessionId::now_v7();
    insert(
        db,
        session::Model {
            user_id: user.id,
            ..rows::live_session(session_id)
        },
    )
    .await;
    let bearer = bearer_for_user(user.id, session_id);
    let token = bearer
        .strip_prefix("Bearer ")
        .expect("a bearer header")
        .to_owned();
    (user.id, token)
}

async fn insert_signature_row_for_stored_copy(
    db: &TestDatabase,
    store_path_hash: &str,
    cache: CacheId,
) {
    let cached_path = db
        .cached_path(store_path_hash)
        .await
        .expect("the stored copy is declared");
    insert(
        db,
        cached_path_signature::Model {
            id: CachedPathSignatureId::now_v7(),
            cached_path: cached_path.id,
            cache,
            signature: Some(b"test-signature".to_vec()),
            created_at: gradient_types::now(),
            ..Default::default()
        },
    )
    .await;
}

async fn seed_two_projects_and_parent_build_that_used_dependency_output_from_private_cache_read_only_by_other_project_and_start_web_server()
-> TestSetupForParentBuildUsingDependencyOutputFromUnreadPrivateCache {
    let (other_project_evaluation, building_project_evaluation) =
        (EvaluationId::now_v7(), EvaluationId::now_v7());
    let (unread_private_cache, building_project_cache) = (CacheId::now_v7(), CacheId::now_v7());
    let db = shape()
        .evaluation(other_project_evaluation)
        .evaluation_status(EvaluationStatus::Completed)
        .derivation(DEPENDENCY_DERIVATION_STORE_PATH)
        .walked()
        .probed()
        .output(DEPENDENCY_OUTPUT_STORE_PATH_HASH)
        .job()
        .build_status(BuildStatus::Completed)
        .shared_build_option(|build| build.fetchable = true)
        .attempt(AttemptOutcome::Built, None)
        .into_cache(unread_private_cache)
        .keyed("signing-key")
        .cached_path(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")
        .mark_uploaded_by_user()
        .evaluation(building_project_evaluation)
        .evaluation_status(EvaluationStatus::Completed)
        .into_cache(building_project_cache)
        .keyed("signing-key")
        .derivation(PARENT_BUILD_DERIVATION_PATH)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(PARENT_BUILD_DERIVATION_PATH)
        .job()
        .output(PARENT_BUILD_OUTPUT_STORE_PATH_HASH)
        .depends_on(DEPENDENCY_DERIVATION_STORE_PATH)
        .build_status(BuildStatus::Completed)
        .attempt(AttemptOutcome::Built, None)
        .cached_path(PARENT_BUILD_OUTPUT_STORE_PATH_HASH, "parent")
        .references(&[(DEPENDENCY_OUTPUT_STORE_PATH_HASH, "dependency")])
        .into_database()
        .await;
    db.execute(&format!(
        "UPDATE cache SET public = true WHERE id = '{}'",
        building_project_cache.into_inner()
    ))
    .await;
    insert_signature_row_for_stored_copy(
        &db,
        DEPENDENCY_OUTPUT_STORE_PATH_HASH,
        unread_private_cache,
    )
    .await;
    insert_signature_row_for_stored_copy(
        &db,
        PARENT_BUILD_OUTPUT_STORE_PATH_HASH,
        building_project_cache,
    )
    .await;
    insert(&db, rows::view_role()).await;

    let (building_project_member, building_project_member_session_token) =
        insert_user_and_login_session_return_user_id_and_session_token(
            &db,
            "member-of-building-project",
        )
        .await;
    insert(
        &db,
        project_user::Model {
            id: ProjectUserId::now_v7(),
            project: db.project_of(building_project_evaluation).await,
            user: building_project_member,
            role: BASE_ROLE_VIEW_ID,
        },
    )
    .await;
    let (other_project_member, other_project_member_session_token) =
        insert_user_and_login_session_return_user_id_and_session_token(
            &db,
            "member-of-other-project",
        )
        .await;
    insert(
        &db,
        project_user::Model {
            id: ProjectUserId::now_v7(),
            project: db.project_of(other_project_evaluation).await,
            user: other_project_member,
            role: BASE_ROLE_VIEW_ID,
        },
    )
    .await;
    insert(
        &db,
        cache_user::Model {
            id: CacheUserId::now_v7(),
            cache: unread_private_cache,
            user: other_project_member,
            role: BASE_CACHE_ROLE_VIEW_ID,
        },
    )
    .await;

    let graph = Graph::new();
    let state = TestState::new()
        .web_db(db.connection())
        .worker_db(db.connection())
        .cache_db(db.connection())
        .graph(Arc::clone(&graph))
        .build();
    gradient_db::graph::consistency::graph_consistency_report(&state.db())
        .await
        .expect("the seed settles");
    graph
        .spawn(state.db(), None, None)
        .await
        .expect("the graph writer starts");
    let publish_round = graph
        .run_publish_round_for_signed_dependency_outputs_of_parent_builds_in_recent_evaluations_until_deadline(
            None,
            gradient_db::caches::demotion::MAX_PUBLISHED_PARENT_BUILDS_PER_SIGNING_PASS,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .await;
    assert!(
        publish_round.failure.is_none(),
        "the publish pass runs: {:?}",
        publish_round.failure
    );
    for store_path_hash in [
        DEPENDENCY_OUTPUT_STORE_PATH_HASH,
        PARENT_BUILD_OUTPUT_STORE_PATH_HASH,
    ] {
        let cached_path = db
            .cached_path(store_path_hash)
            .await
            .expect("the stored copy is declared");
        state
            .nar_storage
            .put(&cached_path.nar_object_storage_key, b"nar".to_vec())
            .await
            .expect("the NAR blob is stored");
    }
    let server = TestServer::new(gradient_web::create_router(state).expect("the router builds"));
    let other_project_dependency_build_job = db
        .job(other_project_evaluation, DEPENDENCY_DERIVATION_STORE_PATH)
        .await
        .expect("the dependency build job of the other project")
        .id;
    TestSetupForParentBuildUsingDependencyOutputFromUnreadPrivateCache {
        db,
        server,
        unread_private_cache_name: format!("cache-{}", unread_private_cache.into_inner().simple()),
        building_project_cache_name: format!(
            "cache-{}",
            building_project_cache.into_inner().simple()
        ),
        other_project_evaluation,
        other_project_dependency_build_job,
        building_project_member_session_token,
        other_project_member_session_token,
    }
}

async fn send_get_request_with_session_token(
    server: &TestServer,
    path: &str,
    session_token: Option<&str>,
) -> (StatusCode, String) {
    let request = server.get(path);
    let response = match session_token {
        Some(token) if path.starts_with("/cache/") => request.add_header(
            "authorization",
            format!("Basic {}", STANDARD.encode(format!("member:{token}"))),
        ),
        Some(token) => request.add_header("authorization", format!("Bearer {token}")),
        None => request,
    }
    .await;
    (response.status_code(), response.text())
}

#[tokio::test]
async fn member_and_anonymous_caller_cannot_read_dependency_output_from_unread_private_cache_but_own_output_is_readable()
 {
    let fixture =
        seed_two_projects_and_parent_build_that_used_dependency_output_from_private_cache_read_only_by_other_project_and_start_web_server()
            .await;
    let (unread, building) = (
        fixture.unread_private_cache_name.as_str(),
        fixture.building_project_cache_name.as_str(),
    );
    let (dependency, parent) = (
        DEPENDENCY_OUTPUT_STORE_PATH_HASH,
        PARENT_BUILD_OUTPUT_STORE_PATH_HASH,
    );
    let (evaluation, build_job) = (
        fixture.other_project_evaluation,
        fixture.other_project_dependency_build_job,
    );
    let routes_with_expected_http_error_status_for_member_and_anonymous_caller = [
        (
            format!("/cache/{building}/{dependency}.narinfo"),
            [404, 404],
        ),
        (
            format!("/cache/{building}/nar/{dependency}.nar"),
            [404, 404],
        ),
        (format!("/cache/{building}/ls/{dependency}"), [404, 404]),
        (
            format!("/cache/{building}/log/{DEPENDENCY_DERIVATION_STORE_PATH}"),
            [404, 404],
        ),
        (
            format!("/api/v1/caches/{building}/nars/{dependency}"),
            [404, 404],
        ),
        (format!("/cache/{unread}/{dependency}.narinfo"), [401, 401]),
        (format!("/cache/{unread}/nar/{dependency}.nar"), [401, 401]),
        (
            format!("/cache/{unread}/log/{DEPENDENCY_DERIVATION_STORE_PATH}"),
            [401, 401],
        ),
        (format!("/api/v1/caches/{unread}/nars"), [404, 404]),
        (format!("/api/v1/evals/{evaluation}"), [404, 404]),
        (format!("/api/v1/evals/{evaluation}/builds"), [404, 404]),
        (format!("/api/v1/builds/{build_job}"), [404, 404]),
        (format!("/api/v1/builds/{build_job}/log"), [404, 404]),
    ];
    let mut observed = Vec::new();
    let mut expected = Vec::new();
    for (caller_index, (caller, auth)) in [
        (
            "member of building project",
            Some(fixture.building_project_member_session_token.as_str()),
        ),
        ("anonymous caller", None),
    ]
    .into_iter()
    .enumerate()
    {
        for (route, refusals) in
            &routes_with_expected_http_error_status_for_member_and_anonymous_caller
        {
            let (status, _) =
                send_get_request_with_session_token(&fixture.server, route, auth).await;
            observed.push((route.clone(), caller, status.as_u16()));
            expected.push((route.clone(), caller, refusals[caller_index]));
        }
        let (status, availability) = send_get_request_with_session_token(
            &fixture.server,
            &format!("/api/v1/caches/{building}/nars/available?hash={dependency}"),
            auth,
        )
        .await;
        let available = serde_json::from_str::<Value>(&availability)
            .expect("the availability answer is JSON")["message"]["available"]
            .clone();
        assert_eq!(
            (status, available),
            (StatusCode::OK, Value::Bool(false)),
            "The cache of the build project reported the dependency output as unavailable to {caller}."
        );
    }
    assert_eq!(
        observed, expected,
        "Routes, callers and statuses for each read of another project's dependency output, private cache and evaluation."
    );

    let (status, nar_list) = send_get_request_with_session_token(
        &fixture.server,
        &format!("/api/v1/caches/{building}/nars"),
        Some(&fixture.building_project_member_session_token),
    )
    .await;
    let listed: Vec<String> = serde_json::from_str::<Value>(&nar_list)
        .expect("the NAR list is JSON")["message"]["items"]
        .as_array()
        .expect("the NAR list has items")
        .iter()
        .map(|item| item["hash"].as_str().unwrap_or_default().to_owned())
        .collect();
    assert_eq!(
        (status, listed),
        (StatusCode::OK, vec![parent.to_owned()]),
        "NAR lists for the cache of the project under build contain only the project's own outputs. Dependency outputs never appear in that list."
    );

    let mut readable = Vec::new();
    for (route, auth) in [
        (
            format!("/cache/{building}/{parent}.narinfo"),
            Some(fixture.building_project_member_session_token.as_str()),
        ),
        (format!("/cache/{building}/{parent}.narinfo"), None),
        (format!("/cache/{building}/nar/{parent}.nar"), None),
        (
            format!("/cache/{unread}/{dependency}.narinfo"),
            Some(fixture.other_project_member_session_token.as_str()),
        ),
        (
            format!("/cache/{unread}/nar/{dependency}.nar"),
            Some(fixture.other_project_member_session_token.as_str()),
        ),
        (
            format!("/api/v1/evals/{evaluation}"),
            Some(fixture.other_project_member_session_token.as_str()),
        ),
    ] {
        let (status, _) = send_get_request_with_session_token(&fixture.server, &route, auth).await;
        readable.push((route, status.as_u16()));
    }
    let all_ok: Vec<(String, u16)> = readable
        .iter()
        .map(|(route, _)| (route.clone(), 200))
        .collect();
    assert_eq!(
        readable, all_ok,
        "The building project's own output and the dependency output for a member of the other project."
    );
    fixture.db.finish().await;
}
