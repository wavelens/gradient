/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::sync::Arc;

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_db::DbContext;
use gradient_entity::build::BuildStatus;
use gradient_entity::evaluation::EvaluationStatus;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::store_path_content_producer::StoredContentVariantProducerKind;
use gradient_entity::{
    cache, cache_subscription_request, cache_user, project, project_cache, project_user,
    store_path_content_producer, user,
};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_fixtures::shape::shape;
use gradient_graph::Graph;
use gradient_scheduler::Scheduler;
use gradient_test_support::state::TestState;
use gradient_test_support::web::bearer;
use gradient_types::SessionId;
use gradient_types::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_ROLE_ADMIN_ID};
use gradient_types::ids::*;
use sea_orm::{ActiveModelBehavior, ActiveModelTrait, EntityTrait, IntoActiveModel};
use serde_json::{Value, json};

struct Instance {
    db: TestDatabase,
    server: TestServer,
    auth: String,
    carol: user::Model,
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

async fn instance_with_project_admin_and_cache_from_server_configuration(
    caller_is_cache_admin: bool,
) -> Instance {
    let db = TestDatabase::create().await;
    insert(&db, rows::user()).await;
    let carol = insert(
        &db,
        user::Model {
            id: UserId::now_v7(),
            username: "carol".into(),
            name: "Carol".into(),
            email: "carol@example.com".into(),
            ..rows::user()
        },
    )
    .await;
    for role in [rows::admin_role(), rows::write_role(), rows::view_role()] {
        insert(&db, role).await;
    }
    insert(&db, rows::project()).await;
    insert(&db, rows::project_member(BASE_ROLE_ADMIN_ID)).await;
    insert(
        &db,
        cache::Model {
            managed: true,
            public: true,
            ..rows::cache()
        },
    )
    .await;
    insert(
        &db,
        cache_user::Model {
            user: carol.id,
            ..rows::cache_member(BASE_CACHE_ROLE_ADMIN_ID)
        },
    )
    .await;
    if caller_is_cache_admin {
        insert(&db, rows::cache_member(BASE_CACHE_ROLE_ADMIN_ID)).await;
    }
    let session_id = SessionId::now_v7();
    insert(&db, rows::live_session(session_id)).await;
    let server = TestState::new().web_db(db.connection()).server();
    Instance {
        db,
        server,
        auth: bearer(session_id),
        carol,
    }
}

async fn request_subscription(instance: &Instance, mode: CacheSubscriptionMode) {
    insert(
        &instance.db,
        cache_subscription_request::Model {
            id: CacheSubscriptionRequestId::now_v7(),
            project: rows::project().id,
            cache: rows::cache().id,
            mode,
            requested_by: instance.carol.id,
            created_at: gradient_types::now(),
        },
    )
    .await;
}

async fn subscriptions(instance: &Instance) -> Vec<project_cache::Model> {
    instance.db.rows::<gradient_types::EProjectCache>().await
}

#[tokio::test]
async fn a_cache_admin_approves_a_request_on_a_configured_cache_with_a_lower_mode() {
    let instance = instance_with_project_admin_and_cache_from_server_configuration(true).await;
    request_subscription(&instance, CacheSubscriptionMode::ReadWrite).await;

    instance
        .server
        .post("/api/v1/caches/test-cache/subscription-requests/test-project")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "mode": "ReadOnly" }))
        .await
        .assert_status_ok();

    let rows = subscriptions(&instance).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].mode, CacheSubscriptionMode::ReadOnly);
    assert!(!rows[0].managed);
    instance.db.finish().await;
}

#[tokio::test]
async fn an_approver_cannot_grant_more_access_than_the_project_requested() {
    let instance = instance_with_project_admin_and_cache_from_server_configuration(true).await;
    request_subscription(&instance, CacheSubscriptionMode::ReadOnly).await;

    instance
        .server
        .post("/api/v1/caches/test-cache/subscription-requests/test-project")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "mode": "ReadWrite" }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    assert!(subscriptions(&instance).await.is_empty());
    instance.db.finish().await;
}

#[tokio::test]
async fn a_subscription_from_the_server_configuration_cannot_be_removed_or_changed() {
    let instance = instance_with_project_admin_and_cache_from_server_configuration(true).await;
    insert(
        &instance.db,
        project_cache::Model {
            project: rows::project().id,
            cache: rows::cache().id,
            managed: true,
            ..Default::default()
        },
    )
    .await;

    instance
        .server
        .delete("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", instance.auth.clone())
        .await
        .assert_status(StatusCode::CONFLICT);
    instance
        .server
        .patch("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "sign_outputs": false }))
        .await
        .assert_status(StatusCode::CONFLICT);

    let rows = subscriptions(&instance).await;
    assert_eq!(rows.len(), 1);
    assert!(rows[0].sign_outputs);
    instance.db.finish().await;
}

#[tokio::test]
async fn turning_off_signing_is_saved_on_the_subscription() {
    let instance = instance_with_project_admin_and_cache_from_server_configuration(true).await;
    insert(
        &instance.db,
        project_cache::Model {
            project: rows::project().id,
            cache: rows::cache().id,
            ..Default::default()
        },
    )
    .await;

    instance
        .server
        .patch("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "sign_outputs": false }))
        .await
        .assert_status_ok();

    assert!(!subscriptions(&instance).await[0].sign_outputs);
    instance.db.finish().await;
}

#[tokio::test]
async fn update_read_only_cache_subscription_with_sign_outputs_returns_bad_request() {
    let instance = instance_with_project_admin_and_cache_from_server_configuration(true).await;
    insert(
        &instance.db,
        project_cache::Model {
            project: rows::project().id,
            cache: rows::cache().id,
            mode: CacheSubscriptionMode::ReadOnly,
            sign_outputs: false,
            ..Default::default()
        },
    )
    .await;

    instance
        .server
        .patch("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "sign_outputs": true }))
        .await
        .assert_status(StatusCode::BAD_REQUEST);

    let rows = subscriptions(&instance).await;
    assert_eq!(rows.len(), 1);
    assert!(!rows[0].sign_outputs);
    instance.db.finish().await;
}

#[tokio::test]
async fn update_cache_subscription_without_sign_outputs_signs_for_write_mode_and_not_for_read_only()
{
    let instance = instance_with_project_admin_and_cache_from_server_configuration(true).await;
    insert(
        &instance.db,
        project_cache::Model {
            project: rows::project().id,
            cache: rows::cache().id,
            mode: CacheSubscriptionMode::ReadWrite,
            sign_outputs: false,
            ..Default::default()
        },
    )
    .await;

    instance
        .server
        .patch("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({}))
        .await
        .assert_status_ok();

    assert!(subscriptions(&instance).await[0].sign_outputs);
    instance.db.finish().await;
}

#[tokio::test]
async fn the_picker_and_the_pending_request_name_the_cache_admins() {
    let instance = instance_with_project_admin_and_cache_from_server_configuration(false).await;

    let picker: Value = instance
        .server
        .get("/api/v1/projects/test-project/subscribable-caches")
        .add_header("authorization", instance.auth.clone())
        .await
        .json();
    assert_eq!(picker["message"][0]["name"], "test-cache");
    assert_eq!(picker["message"][0]["needs_approval"], true);
    assert_eq!(picker["message"][0]["approvers"], json!(["carol"]));

    instance
        .server
        .post("/api/v1/projects/test-project/subscribe/test-cache")
        .add_header("authorization", instance.auth.clone())
        .json(&json!({ "mode": "ReadOnly" }))
        .await
        .assert_status_ok();

    let subscribed: Value = instance
        .server
        .get("/api/v1/projects/test-project/subscribe")
        .add_header("authorization", instance.auth.clone())
        .await
        .json();
    assert_eq!(subscribed["message"][0]["status"], "pending");
    assert_eq!(subscribed["message"][0]["mode"], "ReadOnly");
    assert_eq!(
        subscribed["message"][0]["sign_outputs"], false,
        "A read-only subscription doesn't sign outputs, so the pending request shows no signing."
    );
    assert_eq!(subscribed["message"][0]["approvers"], json!(["carol"]));
    let picker_after: Value = instance
        .server
        .get("/api/v1/projects/test-project/subscribable-caches")
        .add_header("authorization", instance.auth.clone())
        .await
        .json();
    assert_eq!(picker_after["message"], json!([]));
    instance.db.finish().await;
}

const COMPLETED_DEPENDENCY_DERIVATION_PATH: &str = "11111111111111111111111111111111-f.drv";
const COMPLETED_DEPENDENCY_BUILD_F_OUTPUT_STORE_PATH_HASH: &str =
    "f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0f0";
const BUILD_G_WAITING_ON_F_DERIVATION_PATH: &str = "22222222222222222222222222222222-g.drv";
const WAITING_BUILD_G_OUTPUT_STORE_PATH_HASH: &str = "g0g0g0g0g0g0g0g0g0g0g0g0g0g0g0g0";

struct ProjectDeleteWebApiTestSetup {
    db: TestDatabase,
    graph_consistency_check_database_context: DbContext,
    server: TestServer,
    auth: String,
    project_name: String,
}

async fn start_graph_writer_scheduler_without_startup_recount_and_web_test_server_and_sign_in_project_admin(
    db: TestDatabase,
    deleted_project_evaluation_id: EvaluationId,
) -> ProjectDeleteWebApiTestSetup {
    let conn = db.connection();
    let project_id = db.project_of(deleted_project_evaluation_id).await;
    let project = project::Entity::find_by_id(project_id)
        .one(&conn)
        .await
        .expect("the project reads")
        .expect("the project of the evaluation");
    insert(&db, rows::admin_role()).await;
    insert(
        &db,
        project_user::Model {
            project: project_id,
            ..rows::project_member(BASE_ROLE_ADMIN_ID)
        },
    )
    .await;
    let session_id = SessionId::now_v7();
    insert(&db, rows::live_session(session_id)).await;

    let graph = Graph::new();
    let state = TestState::new()
        .web_db(db.connection())
        .worker_db(db.connection())
        .cache_db(db.connection())
        .graph(Arc::clone(&graph))
        .build();
    let database_context = state.db();
    gradient_db::graph::consistency::graph_consistency_report(&database_context)
        .await
        .expect("the seed settles");
    graph
        .spawn(state.db(), None, None)
        .await
        .expect("the graph writer starts");
    let scheduler_without_startup_recount = Arc::new(Scheduler::new(Arc::clone(&state)));
    scheduler_without_startup_recount
        .spawn_core(None)
        .await
        .expect("the scheduler core starts");
    let server = TestServer::new(
        gradient_web::create_router_with_scheduler(
            state,
            scheduler_without_startup_recount,
            gradient_proto::SessionsHandle::new(),
        )
        .expect("the router builds"),
    );
    ProjectDeleteWebApiTestSetup {
        db,
        graph_consistency_check_database_context: database_context,
        server,
        auth: bearer(session_id),
        project_name: project.name,
    }
}

async fn seed_waiting_build_with_input_built_by_project_worker_and_sign_in_project_admin()
-> ProjectDeleteWebApiTestSetup {
    let deleted_project_evaluation_id = EvaluationId::now_v7();
    let db = shape()
        .evaluation(deleted_project_evaluation_id)
        .evaluation_status(EvaluationStatus::Completed)
        .cached_path(COMPLETED_DEPENDENCY_BUILD_F_OUTPUT_STORE_PATH_HASH, "f")
        .evaluation(EvaluationId::now_v7())
        .evaluation_status(EvaluationStatus::Building)
        .derivation(BUILD_G_WAITING_ON_F_DERIVATION_PATH)
        .walked()
        .probed()
        .drv_cached()
        .entry_point(BUILD_G_WAITING_ON_F_DERIVATION_PATH)
        .job()
        .output(WAITING_BUILD_G_OUTPUT_STORE_PATH_HASH)
        .depends_on(COMPLETED_DEPENDENCY_DERIVATION_PATH)
        .build_status(BuildStatus::Created)
        .derivation(COMPLETED_DEPENDENCY_DERIVATION_PATH)
        .walked()
        .probed()
        .drv_cached()
        .job()
        .output(COMPLETED_DEPENDENCY_BUILD_F_OUTPUT_STORE_PATH_HASH)
        .build_status(BuildStatus::Completed)
        .into_database()
        .await;
    start_graph_writer_scheduler_without_startup_recount_and_web_test_server_and_sign_in_project_admin(
        db,
        deleted_project_evaluation_id,
    )
    .await
}

#[tokio::test]
async fn deleting_project_leaves_input_producer_without_worker_and_waiting_build_queued_after_consistency_check()
 {
    let instance =
        seed_waiting_build_with_input_built_by_project_worker_and_sign_in_project_admin().await;
    let build = instance
        .db
        .shared_build(BUILD_G_WAITING_ON_F_DERIVATION_PATH)
        .await;
    assert_eq!(
        (build.status, build.blocking_deps),
        (BuildStatus::Queued, 0),
        "G can start. A worker of the project marked for deletion built F's output."
    );

    instance
        .server
        .delete(&format!("/api/v1/projects/{}", instance.project_name))
        .add_header("authorization", instance.auth.clone())
        .await
        .assert_status_ok();
    gradient_db::graph::consistency::graph_consistency_report(
        &instance.graph_consistency_check_database_context,
    )
    .await
    .expect("the consistency check runs");

    let build = instance
        .db
        .shared_build(BUILD_G_WAITING_ON_F_DERIVATION_PATH)
        .await;
    assert_eq!(
        (build.status, build.blocking_deps),
        (BuildStatus::Queued, 0),
        "G does not leave the queue after the project delete and the consistency check. A worker of the deleted project built the output of F and that output does not lose its accepted status."
    );
    let stored = instance
        .db
        .cached_path(COMPLETED_DEPENDENCY_BUILD_F_OUTPUT_STORE_PATH_HASH)
        .await
        .expect("F's stored output");
    let producers: Vec<_> = instance
        .db
        .rows::<store_path_content_producer::Entity>()
        .await
        .into_iter()
        .filter(|producer| producer.cached_path == stored.id)
        .map(|producer| (producer.kind, producer.worker_registration))
        .collect();
    assert_eq!(
        producers,
        vec![(
            StoredContentVariantProducerKind::BuiltInDispatchedBuildJob,
            None
        )],
        "The producer row for the output of F is present after the project delete. That row is a dispatched build job with no worker reference."
    );
    instance.db.finish().await;
}
