/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum::http::StatusCode;
use axum_test::TestServer;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::{cache, cache_subscription_request, cache_user, project_cache, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
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
