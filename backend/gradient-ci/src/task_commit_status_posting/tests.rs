/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use crate::actions::encrypt_secret_with_file;
use crate::actions::tests::fixtures::make_ci_context_from_worker_and_web_databases;
use gradient_db::git_host_connection_repository_grants::{
    NewGitHostConnectionRepositoryGrant, insert,
};
use gradient_db::rows;
use gradient_entity::{git_host_connection, project, task, team, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::user_id;
use std::io::Write as _;

#[tokio::test]
async fn reporter_for_task_with_an_instance_connection_exists_only_after_a_repository_grant() {
    let database = TestDatabase::create().await;
    let db = database.connection();
    rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
        .await
        .expect("user");
    rows::insert::<team::Entity, _>(&db, gradient_fixtures::rows::team())
        .await
        .expect("team");
    let project = rows::insert::<project::Entity, _>(&db, gradient_fixtures::rows::project())
        .await
        .expect("project");
    let mut crypt_file = tempfile::NamedTempFile::new().expect("crypt file");
    crypt_file
        .write_all(b"test-secret-key-32-bytes-padding!")
        .expect("write crypt file");
    let mut ctx = make_ci_context_from_worker_and_web_databases(db.clone(), db.clone());
    let mut config = (*ctx.db.config).clone();
    config.secrets.crypt_file = crypt_file.path().to_string_lossy().into_owned();
    ctx.db.config = Arc::new(config);
    let access_token = encrypt_secret_with_file(&ctx.db.config.secrets.crypt_file, "token")
        .expect("encrypt access token");
    let connection = rows::insert::<git_host_connection::Entity, _>(
        &db,
        git_host_connection::Model {
            id: GitHostConnectionId::now_v7(),
            name: "instance-github".into(),
            display_name: "GitHub".into(),
            git_host_type: GitHostType::GitHub,
            reports_status: true,
            access_token: Some(access_token),
            created_by: user_id(),
            created_at: gradient_fixtures::test_date(),
            ..Default::default()
        },
    )
    .await
    .expect("instance connection");
    let task = rows::insert::<task::Entity, _>(
        &db,
        task::Model {
            project: project.id,
            repository: "https://github.com/acme/infra".into(),
            git_host_connection: Some(connection.id),
            report_status: true,
            ..gradient_fixtures::rows::task()
        },
    )
    .await
    .expect("task");

    assert!(
        reporter_for_task(&ctx, task.id)
            .await
            .expect("lookup")
            .is_none()
    );

    insert(
        &db,
        NewGitHostConnectionRepositoryGrant {
            connection: connection.id,
            project: project.id,
            repository_owner: "acme",
            repository_name: Some("infra"),
            source: RepositoryGrantSource::Admin,
            created_by: Some(user_id()),
        },
    )
    .await
    .expect("grant");
    assert!(
        reporter_for_task(&ctx, task.id)
            .await
            .expect("lookup")
            .is_some()
    );
    database.finish().await;
}
