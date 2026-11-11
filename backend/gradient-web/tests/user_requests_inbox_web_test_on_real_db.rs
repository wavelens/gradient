/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use axum_test::TestServer;
use gradient_entity::project_cache::CacheSubscriptionMode;
use gradient_entity::team_user::TeamRole;
use gradient_entity::{cache_subscription_request, session, team_project_request, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_test_support::state::TestState;
use gradient_test_support::web::{bearer, bearer_for_user};
use gradient_types::SessionId;
use gradient_types::consts::{BASE_CACHE_ROLE_ADMIN_ID, BASE_ROLE_VIEW_ID};
use gradient_types::ids::*;
use sea_orm::{ActiveModelBehavior, ActiveModelTrait, EntityTrait, IntoActiveModel};
use serde_json::{Value, json};

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

async fn inbox(server: &TestServer, auth: String) -> Value {
    let response = server
        .get("/api/v1/user/requests")
        .add_header("authorization", auth)
        .await;
    assert_eq!(response.status_code(), 200, "{}", response.text());
    response.json::<Value>()["message"].clone()
}

#[tokio::test]
async fn inbox_shows_project_subscription_request_to_project_cache_admin_and_team_grant_request_to_team_admin_but_not_to_unrelated_user()
 {
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
    insert(&db, rows::cache()).await;
    insert(&db, rows::cache_member(BASE_CACHE_ROLE_ADMIN_ID)).await;
    insert(&db, rows::team()).await;
    insert(&db, rows::team_member(TeamRole::Admin)).await;
    insert(
        &db,
        cache_subscription_request::Model {
            id: CacheSubscriptionRequestId::now_v7(),
            project: rows::project().id,
            cache: rows::cache().id,
            mode: CacheSubscriptionMode::ReadOnly,
            requested_by: carol.id,
            created_at: gradient_types::now(),
        },
    )
    .await;
    insert(
        &db,
        team_project_request::Model {
            id: TeamProjectRequestId::now_v7(),
            team: rows::team().id,
            project: rows::project().id,
            role: Some(BASE_ROLE_VIEW_ID),
            includes_users: true,
            includes_workers: true,
            requested_by: Some(carol.id),
            created_at: gradient_types::now(),
        },
    )
    .await;
    let (admin_session, carol_session) = (SessionId::now_v7(), SessionId::now_v7());
    insert(&db, rows::live_session(admin_session)).await;
    insert(
        &db,
        session::Model {
            user_id: carol.id,
            ..rows::live_session(carol_session)
        },
    )
    .await;
    let server = TestState::new().web_db(db.connection()).server();

    let admin = inbox(&server, bearer(admin_session)).await;
    assert_eq!(admin["subscription_requests"][0]["cache"], "test-cache");
    assert_eq!(admin["subscription_requests"][0]["project"], "test-project");
    assert_eq!(admin["subscription_requests"][0]["mode"], "ReadOnly");
    assert_eq!(admin["subscription_requests"][0]["requested_by"], "carol");
    assert_eq!(admin["team_requests"][0]["team"], "platform");
    assert_eq!(admin["team_requests"][0]["target"], "test-project");
    assert_eq!(admin["team_requests"][0]["workers"], true);

    let unrelated = inbox(&server, bearer_for_user(carol.id, carol_session)).await;
    assert_eq!(
        unrelated,
        json!({ "invites": [], "subscription_requests": [], "team_requests": [] })
    );
    db.finish().await;
}
