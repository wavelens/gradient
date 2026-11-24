/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::postgres_state_apply_account_identity_tests::TemporaryCredentialsDirectoryAndCryptSecretWithStateApply;
use base64::{Engine, engine::general_purpose};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::rows;
use gradient_types::*;
use sea_orm::{ActiveModelTrait, DatabaseConnection, IntoActiveModel};
use serde_json::{Value, json};

struct StateFileDeclaredProjectTaskCacheUuids {
    project: ProjectId,
    task: TaskId,
    cache: CacheId,
}

impl StateFileDeclaredProjectTaskCacheUuids {
    fn new() -> Self {
        Self {
            project: ProjectId::now_v7(),
            task: TaskId::now_v7(),
            cache: CacheId::now_v7(),
        }
    }
}

fn build_state_file_json_with_one_admin_user_one_project_one_task_and_one_cache(
    project: &str,
    task: &str,
    cache: &str,
    ids: &StateFileDeclaredProjectTaskCacheUuids,
) -> Value {
    json!({
        "users": {
            "admin": { "username": "admin", "name": "Admin", "email": "admin@x.io" }
        },
        "projects": {
            project: {
                "name": project, "display_name": "ACME", "id": ids.project.to_string(),
                "private_key_file": "k", "public": false, "created_by": "admin"
            }
        },
        "tasks": {
            task: {
                "name": task, "project": project, "display_name": "Web",
                "id": ids.task.to_string(),
                "repository": "https://example.com/acme/web.git", "created_by": "admin"
            }
        },
        "caches": {
            cache: {
                "name": cache, "display_name": "Main", "id": ids.cache.to_string(),
                "signing_key_file": "k", "public": false, "created_by": "admin"
            }
        }
    })
}

async fn create_empty_test_postgres_database_with_built_in_admin_role_row() -> TestDatabase {
    let test_db = TestDatabase::create().await;
    rows::admin_role()
        .into_active_model()
        .insert(&test_db.connection())
        .await
        .unwrap();
    test_db
}

fn create_temp_credentials_dir_with_project_ssh_keys_and_cache_signing_keys(
    projects: &[&str],
    caches: &[&str],
) -> TemporaryCredentialsDirectoryAndCryptSecretWithStateApply {
    let state = TemporaryCredentialsDirectoryAndCryptSecretWithStateApply::new();
    for project in projects {
        state.write_project_private_key(project);
    }
    for cache in caches {
        state.write(
            "cache",
            cache,
            "signing_key",
            &general_purpose::STANDARD.encode([9u8; 64]),
        );
    }
    state
}

async fn apply(
    state: &TemporaryCredentialsDirectoryAndCryptSecretWithStateApply,
    db: &DatabaseConnection,
    config: &Value,
) -> Result<(), String> {
    state
        .apply(db, &config.to_string(), true)
        .await
        .map_err(|e| e.to_string())
}

#[tokio::test]
async fn renaming_project_cache_and_task_with_declared_id_keeps_their_rows_and_ids_when_delete_state_is_enabled()
 {
    let test_db = create_empty_test_postgres_database_with_built_in_admin_role_row().await;
    let db = test_db.connection();
    let ids = StateFileDeclaredProjectTaskCacheUuids::new();
    let state = create_temp_credentials_dir_with_project_ssh_keys_and_cache_signing_keys(
        &["acme", "acme-corp"],
        &["somecache", "some-cache"],
    );

    let before = build_state_file_json_with_one_admin_user_one_project_one_task_and_one_cache(
        "acme",
        "web",
        "somecache",
        &ids,
    );
    apply(&state, &db, &before).await.unwrap();
    let after = build_state_file_json_with_one_admin_user_one_project_one_task_and_one_cache(
        "acme-corp",
        "website",
        "some-cache",
        &ids,
    );
    apply(&state, &db, &after).await.unwrap();

    let projects = test_db.rows::<EProject>().await;
    assert_eq!(projects.len(), 1, "{projects:?}");
    assert_eq!(
        (projects[0].id, projects[0].name.as_str()),
        (ids.project, "acme-corp")
    );
    let tasks = test_db.rows::<ETask>().await;
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!((tasks[0].id, tasks[0].name.as_str()), (ids.task, "website"));
    let caches = test_db.rows::<ECache>().await;
    assert_eq!(caches.len(), 1, "{caches:?}");
    assert_eq!(
        (caches[0].id, caches[0].name.as_str(), caches[0].managed),
        (ids.cache, "some-cache", true)
    );
    test_db.finish().await;
}

#[tokio::test]
async fn cache_rename_with_declared_id_frees_old_name_and_new_entry_gets_new_row() {
    let test_db = create_empty_test_postgres_database_with_built_in_admin_role_row().await;
    let db = test_db.connection();
    let ids = StateFileDeclaredProjectTaskCacheUuids::new();
    let state = create_temp_credentials_dir_with_project_ssh_keys_and_cache_signing_keys(
        &["acme"],
        &["somecache", "some-cache"],
    );

    let before = build_state_file_json_with_one_admin_user_one_project_one_task_and_one_cache(
        "acme",
        "web",
        "somecache",
        &ids,
    );
    apply(&state, &db, &before).await.unwrap();
    let mut after = build_state_file_json_with_one_admin_user_one_project_one_task_and_one_cache(
        "acme",
        "web",
        "some-cache",
        &ids,
    );
    after["caches"]["somecache"] = json!({
        "name": "somecache", "display_name": "Fresh",
        "signing_key_file": "k", "public": false, "created_by": "admin"
    });
    apply(&state, &db, &after).await.unwrap();

    let caches = test_db.rows::<ECache>().await;
    assert_eq!(caches.len(), 2, "{caches:?}");
    let renamed = caches.iter().find(|cache| cache.id == ids.cache).unwrap();
    assert_eq!(renamed.name, "some-cache");
    let fresh = caches.iter().find(|cache| cache.id != ids.cache).unwrap();
    assert_eq!(
        (fresh.name.as_str(), fresh.display_name.as_str()),
        ("somecache", "Fresh")
    );
    test_db.finish().await;
}

#[tokio::test]
async fn apply_fails_for_task_declaring_id_of_task_in_another_project() {
    let test_db = create_empty_test_postgres_database_with_built_in_admin_role_row().await;
    let db = test_db.connection();
    let ids = StateFileDeclaredProjectTaskCacheUuids::new();
    let state = create_temp_credentials_dir_with_project_ssh_keys_and_cache_signing_keys(
        &["acme", "other"],
        &["main"],
    );

    let mut config = build_state_file_json_with_one_admin_user_one_project_one_task_and_one_cache(
        "acme", "web", "main", &ids,
    );
    apply(&state, &db, &config).await.unwrap();
    config["projects"]["other"] = json!({
        "name": "other", "display_name": "Other",
        "private_key_file": "k", "public": false, "created_by": "admin"
    });
    config["tasks"]["web"]["project"] = json!("other");

    let err = apply(&state, &db, &config).await.unwrap_err();

    assert!(err.contains(&ids.task.to_string()), "{err}");
    let tasks = test_db.rows::<ETask>().await;
    assert_eq!(tasks.len(), 1, "{tasks:?}");
    assert_eq!(tasks[0].project, ids.project);
    test_db.finish().await;
}

#[tokio::test]
async fn apply_points_internal_upstream_cache_to_cache_renamed_in_same_apply() {
    let test_db = create_empty_test_postgres_database_with_built_in_admin_role_row().await;
    let db = test_db.connection();
    let ids = StateFileDeclaredProjectTaskCacheUuids::new();
    let renamed_id = CacheId::now_v7();
    let state = create_temp_credentials_dir_with_project_ssh_keys_and_cache_signing_keys(
        &["acme"],
        &["a", "b", "b2"],
    );
    let cache_b = |name: &str| {
        json!({
            "name": name, "display_name": "B", "id": renamed_id.to_string(),
            "signing_key_file": "k", "public": false, "created_by": "admin"
        })
    };

    let mut before = build_state_file_json_with_one_admin_user_one_project_one_task_and_one_cache(
        "acme", "web", "a", &ids,
    );
    before["caches"]["b"] = cache_b("b");
    apply(&state, &db, &before).await.unwrap();
    let mut after = build_state_file_json_with_one_admin_user_one_project_one_task_and_one_cache(
        "acme", "web", "a", &ids,
    );
    after["caches"]["b2"] = cache_b("b2");
    after["caches"]["a"]["upstream_caches"] = json!([{ "type": "internal", "cache_name": "b2" }]);
    apply(&state, &db, &after).await.unwrap();

    let upstreams_of_a: Vec<_> = test_db
        .rows::<ECacheUpstream>()
        .await
        .into_iter()
        .filter(|upstream| upstream.cache == ids.cache)
        .map(|upstream| upstream.upstream_cache)
        .collect();
    assert_eq!(upstreams_of_a, vec![Some(renamed_id)]);
    test_db.finish().await;
}
