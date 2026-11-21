/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use gradient_entity::{git_host_connection, project, task, task_action, team, user};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{team_id, user_id};
use gradient_migration::{Migrator, MigratorTrait};
use sea_orm::DatabaseConnection;

const GIT_HOST_CONNECTION_REPOSITORY_GRANT_TABLE_UPGRADE_MIGRATION_NAME: &str = "m20261229_000000_create_git_host_connection_repository_grant_table_and_grant_shared_connections_to_the_repositories_of_tasks_already_using_them";

async fn create_test_database_with_user_and_two_teams() -> (TestDatabase, DatabaseConnection, TeamId)
{
    let database = TestDatabase::create().await;
    let db = database.connection();
    crate::rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
        .await
        .expect("user");
    crate::rows::insert::<team::Entity, _>(&db, gradient_fixtures::rows::team())
        .await
        .expect("team");
    let other_team = TeamId::now_v7();
    crate::rows::insert::<team::Entity, _>(
        &db,
        team::Model {
            id: other_team,
            name: "other-team".into(),
            ..gradient_fixtures::rows::team()
        },
    )
    .await
    .expect("other team");
    (database, db, other_team)
}

async fn insert_project(db: &DatabaseConnection, owner_team: Option<TeamId>) -> MProject {
    let id = ProjectId::now_v7();
    crate::rows::insert::<project::Entity, _>(
        db,
        project::Model {
            id,
            name: format!("project-{id}"),
            owner_team,
            ..gradient_fixtures::rows::project()
        },
    )
    .await
    .expect("project")
}

async fn insert_connection(
    db: &DatabaseConnection,
    owner_team: Option<TeamId>,
) -> MGitHostConnection {
    let id = GitHostConnectionId::now_v7();
    crate::rows::insert::<git_host_connection::Entity, _>(
        db,
        git_host_connection::Model {
            id,
            owner_team,
            name: format!("connection-{id}"),
            display_name: "connection".into(),
            git_host_type: GitHostType::GitHub,
            created_by: user_id(),
            created_at: gradient_fixtures::test_date(),
            ..Default::default()
        },
    )
    .await
    .expect("connection")
}

async fn insert_task(
    db: &DatabaseConnection,
    project: &MProject,
    repository: &str,
    connection: Option<&MGitHostConnection>,
) -> MTask {
    let id = TaskId::now_v7();
    crate::rows::insert::<task::Entity, _>(
        db,
        task::Model {
            id,
            project: project.id,
            name: format!("task-{id}"),
            repository: repository.into(),
            git_host_connection: connection.map(|c| c.id),
            report_status: connection.is_some(),
            ..gradient_fixtures::rows::task()
        },
    )
    .await
    .expect("task")
}

async fn insert_open_pull_request_task_action_for_task_with_git_host_connection_and_is_active(
    db: &DatabaseConnection,
    task: &MTask,
    connection: &MGitHostConnection,
    active: bool,
) {
    let id = TaskActionId::now_v7();
    crate::rows::insert::<task_action::Entity, _>(
        db,
        task_action::Model {
            id,
            task: task.id,
            name: format!("open-pr-{id}"),
            action_type: task_action::ActionType::OpenPr,
            config: serde_json::json!({ "git_host_connection_id": connection.id }),
            events: serde_json::json!([]),
            active,
            created_by: user_id(),
            created_at: gradient_fixtures::test_date(),
            updated_at: gradient_fixtures::test_date(),
            ..Default::default()
        },
    )
    .await
    .expect("open_pr action");
}

async fn revert_repository_grant_migration_and_later_migrations(db: &DatabaseConnection) {
    let migrations = Migrator::migrations();
    let position = migrations
        .iter()
        .position(|m| m.name() == GIT_HOST_CONNECTION_REPOSITORY_GRANT_TABLE_UPGRADE_MIGRATION_NAME)
        .expect("the grant migration is registered");
    let steps = u32::try_from(migrations.len() - position).expect("the count fits in u32");
    Migrator::down(db, Some(steps))
        .await
        .expect("the grant migration reverts");
}

async fn revert_repository_grant_migration_and_run_all_migrations_up(db: &DatabaseConnection) {
    revert_repository_grant_migration_and_later_migrations(db).await;
    Migrator::up(db, None)
        .await
        .expect("the grant migration runs");
}

fn repository_grant_row_to_comparison_tuple(
    grant: &MGitHostConnectionRepositoryGrant,
) -> (GitHostConnectionId, ProjectId, &str, Option<&str>) {
    (
        grant.git_host_connection,
        grant.project,
        grant.repository_owner.as_str(),
        grant.repository_name.as_deref(),
    )
}

#[tokio::test]
async fn migration_creates_lowercase_repository_grant_for_each_task_using_shared_git_host_connection()
 {
    let (database, db, _) = create_test_database_with_user_and_two_teams().await;
    let project = insert_project(&db, None).await;
    let other_project = insert_project(&db, None).await;
    let instance_connection = insert_connection(&db, None).await;
    let task = insert_task(
        &db,
        &project,
        "git@GitHub.com:Acme/Infra.git",
        Some(&instance_connection),
    )
    .await;
    insert_task(&db, &other_project, "https://github.com/acme/other", None).await;

    revert_repository_grant_migration_and_run_all_migrations_up(&db).await;

    let grants = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    assert_eq!(
        grants
            .iter()
            .map(repository_grant_row_to_comparison_tuple)
            .collect::<Vec<_>>(),
        [(instance_connection.id, project.id, "acme", Some("infra"))]
    );
    assert_eq!(grants[0].source, RepositoryGrantSource::Upgrade);
    assert_eq!(grants[0].created_by, None);
    let after = database.find::<ETask>(task.id).await.expect("task");
    assert_eq!(after, task);
    database.finish().await;
}

#[tokio::test]
async fn active_open_pull_request_task_action_gets_repository_grant_and_inactive_gets_none() {
    let (database, db, _) = create_test_database_with_user_and_two_teams().await;
    let project = insert_project(&db, None).await;
    let instance_connection = insert_connection(&db, None).await;
    let task_with_active_action =
        insert_task(&db, &project, "https://github.com/acme/infra", None).await;
    let task_with_inactive_action =
        insert_task(&db, &project, "https://github.com/acme/secrets", None).await;
    insert_open_pull_request_task_action_for_task_with_git_host_connection_and_is_active(
        &db,
        &task_with_active_action,
        &instance_connection,
        true,
    )
    .await;
    insert_open_pull_request_task_action_for_task_with_git_host_connection_and_is_active(
        &db,
        &task_with_inactive_action,
        &instance_connection,
        false,
    )
    .await;

    revert_repository_grant_migration_and_run_all_migrations_up(&db).await;

    let grants = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    assert_eq!(
        grants
            .iter()
            .map(repository_grant_row_to_comparison_tuple)
            .collect::<Vec<_>>(),
        [(instance_connection.id, project.id, "acme", Some("infra"))]
    );
    database.finish().await;
}

#[tokio::test]
async fn upgrade_migration_grants_state_file_task_and_hand_created_task_same_way() {
    let (database, db, _) = create_test_database_with_user_and_two_teams().await;
    let project = insert_project(&db, None).await;
    let instance_connection = insert_connection(&db, None).await;
    let state_file_task = insert_task(
        &db,
        &project,
        "https://github.com/acme/infra",
        Some(&instance_connection),
    )
    .await;
    let mut managed: task::ActiveModel = state_file_task.into();
    managed.managed = Set(true);
    managed.update(&db).await.expect("mark the task as managed");
    insert_task(
        &db,
        &project,
        "https://github.com/acme/website",
        Some(&instance_connection),
    )
    .await;

    revert_repository_grant_migration_and_run_all_migrations_up(&db).await;

    let grants = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    let mut keys: Vec<_> = grants
        .iter()
        .map(repository_grant_row_to_comparison_tuple)
        .collect();
    keys.sort();
    let mut expected = vec![
        (instance_connection.id, project.id, "acme", Some("infra")),
        (instance_connection.id, project.id, "acme", Some("website")),
    ];
    expected.sort();
    assert_eq!(keys, expected);
    assert!(
        grants
            .iter()
            .all(|grant| grant.source == RepositoryGrantSource::Upgrade)
    );
    database.finish().await;
}

#[tokio::test]
async fn upgrade_migration_team_git_host_connection_gets_no_repository_grant_for_project_outside_team()
 {
    let (database, db, other_team) = create_test_database_with_user_and_two_teams().await;
    let project_of_the_team = insert_project(&db, Some(team_id())).await;
    let project_of_other_team = insert_project(&db, Some(other_team)).await;
    let team_connection = insert_connection(&db, Some(team_id())).await;
    insert_task(
        &db,
        &project_of_the_team,
        "https://github.com/acme/infra",
        Some(&team_connection),
    )
    .await;
    let outside_task = insert_task(
        &db,
        &project_of_other_team,
        "https://github.com/acme/infra",
        Some(&team_connection),
    )
    .await;
    insert_open_pull_request_task_action_for_task_with_git_host_connection_and_is_active(
        &db,
        &outside_task,
        &team_connection,
        true,
    )
    .await;

    revert_repository_grant_migration_and_run_all_migrations_up(&db).await;

    let grants = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    assert_eq!(
        grants
            .iter()
            .map(repository_grant_row_to_comparison_tuple)
            .collect::<Vec<_>>(),
        [(
            team_connection.id,
            project_of_the_team.id,
            "acme",
            Some("infra")
        )]
    );
    database.finish().await;
}

#[tokio::test]
async fn upgrade_detaches_task_when_repository_url_has_no_readable_owner_and_name() {
    let (database, db, _) = create_test_database_with_user_and_two_teams().await;
    let project = insert_project(&db, None).await;
    let instance_connection = insert_connection(&db, None).await;
    let attached = insert_task(
        &db,
        &project,
        "https://github.com/acme",
        Some(&instance_connection),
    )
    .await;
    let with_action_only = insert_task(&db, &project, "https://github.com/acme", None).await;
    insert_open_pull_request_task_action_for_task_with_git_host_connection_and_is_active(
        &db,
        &with_action_only,
        &instance_connection,
        true,
    )
    .await;

    revert_repository_grant_migration_and_run_all_migrations_up(&db).await;

    assert_eq!(
        database.find::<ETask>(attached.id).await.expect("task"),
        MTask {
            git_host_connection: None,
            report_status: false,
            ..attached
        }
    );
    assert_eq!(
        database
            .find::<ETask>(with_action_only.id)
            .await
            .expect("task"),
        with_action_only
    );
    assert!(
        database
            .rows::<EGitHostConnectionRepositoryGrant>()
            .await
            .is_empty()
    );
    database.finish().await;
}

#[tokio::test]
async fn repository_grant_migration_runs_on_database_with_no_tasks() {
    let (database, db, _) = create_test_database_with_user_and_two_teams().await;

    revert_repository_grant_migration_and_run_all_migrations_up(&db).await;

    assert!(
        database
            .rows::<EGitHostConnectionRepositoryGrant>()
            .await
            .is_empty()
    );
    database.finish().await;
}

#[tokio::test]
async fn repository_grant_migration_can_be_reverted_and_run_again() {
    let (database, db, _) = create_test_database_with_user_and_two_teams().await;
    let project = insert_project(&db, None).await;
    let instance_connection = insert_connection(&db, None).await;
    insert_task(
        &db,
        &project,
        "https://github.com/acme/infra",
        Some(&instance_connection),
    )
    .await;

    revert_repository_grant_migration_and_run_all_migrations_up(&db).await;
    revert_repository_grant_migration_and_run_all_migrations_up(&db).await;

    let grants = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    assert_eq!(
        grants
            .iter()
            .map(repository_grant_row_to_comparison_tuple)
            .collect::<Vec<_>>(),
        [(instance_connection.id, project.id, "acme", Some("infra"))]
    );
    database.finish().await;
}
