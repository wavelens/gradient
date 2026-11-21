/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use super::*;
use gradient_db::git_host_connection_repository_grants::{
    NewGitHostConnectionRepositoryGrant, insert,
    load_task_git_host_connection_project_and_grants_and_check_repository_access,
};
use gradient_fixtures::database::TestDatabase;
use gradient_fixtures::ids::{team_id, user_id};
use gradient_fixtures::rows;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};

const TASK_NAME: &str = "deploy";

struct Setup {
    database: TestDatabase,
    db: DatabaseConnection,
    project: MProject,
}

async fn setup(project_owner_team: Option<TeamId>) -> Setup {
    let database = TestDatabase::create().await;
    let db = database.connection();
    user::Model {
        account_origin: AccountOrigin::DeclaredInStateFile,
        managed: true,
        ..rows::user()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    rows::team().into_active_model().insert(&db).await.unwrap();
    let project = project::Model {
        owner_team: project_owner_team,
        ..rows::project_with_id(ProjectId::now_v7(), "infra-project")
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();
    Setup {
        database,
        db,
        project,
    }
}

async fn insert_shared_connection(
    db: &DatabaseConnection,
    owner_team: Option<TeamId>,
    git_host_type: GitHostType,
) -> MGitHostConnection {
    git_host_connection::Model {
        id: GitHostConnectionId::now_v7(),
        owner_team,
        name: "shared".into(),
        display_name: "Shared".into(),
        git_host_type,
        created_by: user_id(),
        created_at: gradient_fixtures::test_date(),
        ..Default::default()
    }
    .into_active_model()
    .insert(db)
    .await
    .unwrap()
}

fn state_tasks(project: &MProject, repository: &str) -> HashMap<String, StateTask> {
    let task: StateTask = serde_json::from_value(serde_json::json!({
        "name": TASK_NAME,
        "project": project.name,
        "display_name": "Deploy",
        "repository": repository,
        "created_by": rows::user().username,
        "git_host_connection": "shared",
    }))
    .unwrap();
    HashMap::from([(TASK_NAME.to_owned(), task)])
}

fn build_state_tasks_with_one_task_using_git_host_connection_only_for_open_pr(
    project: &MProject,
    repository: &str,
) -> HashMap<String, StateTask> {
    let task: StateTask = serde_json::from_value(serde_json::json!({
        "name": TASK_NAME,
        "project": project.name,
        "display_name": "Deploy",
        "repository": repository,
        "created_by": rows::user().username,
        "actions": [{
            "name": "flake-update",
            "type": "open_pr",
            "config": { "git_host_connection": "shared" },
        }],
    }))
    .unwrap();
    HashMap::from([(TASK_NAME.to_owned(), task)])
}

async fn assert_no_task_rows_and_no_repository_grant_rows(database: &TestDatabase) {
    assert!(database.rows::<ETask>().await.is_empty());
    assert!(
        database
            .rows::<EGitHostConnectionRepositoryGrant>()
            .await
            .is_empty()
    );
}

async fn apply(db: &DatabaseConnection, tasks: &HashMap<String, StateTask>) -> Result<(), String> {
    let credentials_dir =
        std::env::temp_dir().join(format!("gradient-state-test-{}", uuid::Uuid::now_v7()));
    std::fs::create_dir_all(&credentials_dir).unwrap();
    let crypt_file = credentials_dir.join("crypt");
    std::fs::write(&crypt_file, "0123456789abcdef0123456789abcdef").unwrap();
    let transaction = db.begin().await.unwrap();
    let app = StateApplicator {
        db: &transaction,
        crypt_secret_file: crypt_file.to_str().unwrap(),
        credentials_dir: &credentials_dir,
        email_enabled: false,
        user_url_destination_allowlist: &Default::default(),
    };
    let result = app.apply_tasks(tasks).await.map_err(|e| e.to_string());
    if result.is_ok() {
        transaction.commit().await.unwrap();
    }
    result
}

async fn grant_with_source(
    db: &DatabaseConnection,
    connection: &MGitHostConnection,
    project: &MProject,
    repository_name: &str,
    source: RepositoryGrantSource,
) -> MGitHostConnectionRepositoryGrant {
    insert(
        db,
        NewGitHostConnectionRepositoryGrant {
            connection: connection.id,
            project: project.id,
            repository_owner: "acme",
            repository_name: Some(repository_name),
            source,
            created_by: None,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_state_file_task_with_an_instance_connection_gets_a_state_file_grant_for_exactly_its_repository()
 {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    let connection = insert_shared_connection(&db, None, GitHostType::GitHub).await;

    apply(&db, &state_tasks(&project, "https://github.com/Acme/Infra"))
        .await
        .unwrap();

    let grants = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].git_host_connection, connection.id);
    assert_eq!(grants[0].project, project.id);
    assert_eq!(grants[0].repository_owner, "acme");
    assert_eq!(grants[0].repository_name.as_deref(), Some("infra"));
    assert_eq!(grants[0].source, RepositoryGrantSource::StateFile);
    let task = database.rows::<ETask>().await.remove(0);
    let allowed =
        load_task_git_host_connection_project_and_grants_and_check_repository_access(&db, &task)
            .await
            .unwrap()
            .expect("the state file grant lets the connection act on the task repository");
    assert_eq!(allowed.connection().id, connection.id);
    database.finish().await;
}

#[tokio::test]
async fn state_file_managed_task_creates_repository_grant_unmanaged_task_keeps_no_connection_and_unchanged_report_status()
 {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    let connection = insert_shared_connection(&db, None, GitHostType::GitHub).await;
    let unmanaged = task::Model {
        id: TaskId::now_v7(),
        project: project.id,
        name: "manual".into(),
        repository: "https://github.com/acme/app".into(),
        managed: false,
        git_host_connection: None,
        ..rows::task()
    }
    .into_active_model()
    .insert(&db)
    .await
    .unwrap();

    apply(&db, &state_tasks(&project, "https://github.com/acme/app"))
        .await
        .unwrap();

    let grants = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    assert_eq!(
        grants
            .iter()
            .map(|grant| (
                grant.git_host_connection,
                grant.repository_name.as_deref(),
                grant.source
            ))
            .collect::<Vec<_>>(),
        [(connection.id, Some("app"), RepositoryGrantSource::StateFile)],
        "A state file can create a grant for the repository of the managed task."
    );
    let after = database.find::<ETask>(unmanaged.id).await.unwrap();
    assert_eq!(after.git_host_connection, None);
    assert_eq!(
        after, unmanaged,
        "Grants from the state file link no task. The unmanaged task row does not change, no connection and the report status does not change."
    );
    database.finish().await;
}

#[tokio::test]
async fn a_second_reconciliation_of_the_same_state_file_keeps_the_grant_unchanged() {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    insert_shared_connection(&db, None, GitHostType::GitHub).await;
    let tasks = state_tasks(&project, "https://github.com/acme/infra");

    apply(&db, &tasks).await.unwrap();
    let first = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    apply(&db, &tasks).await.unwrap();
    let second = database.rows::<EGitHostConnectionRepositoryGrant>().await;

    assert_eq!(first.len(), 1);
    assert_eq!(first, second);
    database.finish().await;
}

#[tokio::test]
async fn removing_the_task_from_the_state_file_removes_only_its_state_file_grant() {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    let connection = insert_shared_connection(&db, None, GitHostType::GitHub).await;
    let admin_grant = grant_with_source(
        &db,
        &connection,
        &project,
        "website",
        RepositoryGrantSource::Admin,
    )
    .await;
    let upgrade_grant = grant_with_source(
        &db,
        &connection,
        &project,
        "docs",
        RepositoryGrantSource::Upgrade,
    )
    .await;
    apply(&db, &state_tasks(&project, "https://github.com/acme/infra"))
        .await
        .unwrap();
    assert_eq!(
        database
            .rows::<EGitHostConnectionRepositoryGrant>()
            .await
            .len(),
        3
    );

    apply(&db, &HashMap::new()).await.unwrap();

    let mut remaining: Vec<GitHostConnectionRepositoryGrantId> = database
        .rows::<EGitHostConnectionRepositoryGrant>()
        .await
        .into_iter()
        .map(|grant| grant.id)
        .collect();
    remaining.sort();
    let mut expected = vec![admin_grant.id, upgrade_grant.id];
    expected.sort();
    assert_eq!(remaining, expected);
    database.finish().await;
}

#[tokio::test]
async fn a_team_connection_for_a_project_outside_the_team_stops_the_reconciliation_and_stores_nothing()
 {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    insert_shared_connection(&db, Some(team_id()), GitHostType::GitHub).await;

    let error = apply(&db, &state_tasks(&project, "https://github.com/acme/infra"))
        .await
        .unwrap_err();

    assert!(error.contains("Task 'deploy'"), "{error}");
    assert!(error.contains("isn't available to project"), "{error}");
    assert!(database.rows::<ETask>().await.is_empty());
    assert!(
        database
            .rows::<EGitHostConnectionRepositoryGrant>()
            .await
            .is_empty()
    );
    database.finish().await;
}

#[tokio::test]
async fn a_gitea_connection_without_a_base_url_stops_the_reconciliation_and_stores_nothing() {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    db.execute_unprepared(
        "ALTER TABLE git_host_connection DROP CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url",
    )
    .await
    .unwrap();
    insert_shared_connection(&db, None, GitHostType::Gitea).await;
    db.execute_unprepared(
        "ALTER TABLE git_host_connection ADD CONSTRAINT git_host_connection_gitea_forgejo_and_gitlab_need_base_url \
         CHECK (git_host_type NOT IN (0, 1, 2) OR base_url IS NOT NULL) NOT VALID",
    )
    .await
    .unwrap();

    let error = apply(
        &db,
        &state_tasks(&project, "https://gitea.example/acme/infra"),
    )
    .await
    .unwrap_err();

    assert!(error.contains("Task 'deploy'"), "{error}");
    assert!(error.contains("no base URL"), "{error}");
    assert!(database.rows::<ETask>().await.is_empty());
    assert!(
        database
            .rows::<EGitHostConnectionRepositoryGrant>()
            .await
            .is_empty()
    );
    database.finish().await;
}

#[tokio::test]
async fn test_upgrade_repository_grant_for_state_file_task_becomes_state_file_grant_and_is_removed_when_task_leaves_state_file()
 {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    let connection = insert_shared_connection(&db, None, GitHostType::GitHub).await;
    let upgrade_grant = grant_with_source(
        &db,
        &connection,
        &project,
        "infra",
        RepositoryGrantSource::Upgrade,
    )
    .await;

    apply(&db, &state_tasks(&project, "https://github.com/acme/infra"))
        .await
        .unwrap();
    let taken_over = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    assert_eq!(taken_over.len(), 1);
    assert_eq!(taken_over[0].id, upgrade_grant.id);
    assert_eq!(taken_over[0].source, RepositoryGrantSource::StateFile);

    apply(&db, &HashMap::new()).await.unwrap();

    assert!(
        database
            .rows::<EGitHostConnectionRepositoryGrant>()
            .await
            .is_empty()
    );
    database.finish().await;
}

#[tokio::test]
async fn admin_repository_grant_stays_after_state_file_task_leaves() {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    let connection = insert_shared_connection(&db, None, GitHostType::GitHub).await;
    let admin_grant = grant_with_source(
        &db,
        &connection,
        &project,
        "infra",
        RepositoryGrantSource::Admin,
    )
    .await;

    apply(&db, &state_tasks(&project, "https://github.com/acme/infra"))
        .await
        .unwrap();
    assert_eq!(
        database.rows::<EGitHostConnectionRepositoryGrant>().await,
        std::slice::from_ref(&admin_grant)
    );
    apply(&db, &HashMap::new()).await.unwrap();

    assert_eq!(
        database.rows::<EGitHostConnectionRepositoryGrant>().await,
        [admin_grant]
    );
    database.finish().await;
}

#[tokio::test]
async fn git_host_connection_used_only_by_open_pr_action_gets_state_file_task_repository_grant() {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    let connection = insert_shared_connection(&db, None, GitHostType::GitHub).await;

    apply(
        &db,
        &build_state_tasks_with_one_task_using_git_host_connection_only_for_open_pr(
            &project,
            "https://github.com/acme/infra",
        ),
    )
    .await
    .unwrap();

    let grants = database.rows::<EGitHostConnectionRepositoryGrant>().await;
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].git_host_connection, connection.id);
    assert_eq!(grants[0].repository_owner, "acme");
    assert_eq!(grants[0].repository_name.as_deref(), Some("infra"));
    assert_eq!(grants[0].source, RepositoryGrantSource::StateFile);
    database.finish().await;
}

#[tokio::test]
async fn applying_state_file_for_task_repository_grant_fails_and_stores_nothing_when_open_pr_action_connection_is_on_another_git_host()
 {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    insert_shared_connection(&db, None, GitHostType::GitHub).await;

    let error = apply(
        &db,
        &build_state_tasks_with_one_task_using_git_host_connection_only_for_open_pr(
            &project,
            "https://gitlab.com/acme/infra",
        ),
    )
    .await
    .unwrap_err();

    assert!(error.contains("Task 'deploy'"), "{error}");
    assert!(error.contains("differ"), "{error}");
    assert_no_task_rows_and_no_repository_grant_rows(&database).await;
    database.finish().await;
}

#[tokio::test]
async fn task_git_host_on_different_host_than_repository_stops_reconciliation_and_stores_nothing() {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    insert_shared_connection(&db, None, GitHostType::GitHub).await;

    let error = apply(&db, &state_tasks(&project, "https://gitlab.com/acme/infra"))
        .await
        .unwrap_err();

    assert!(error.contains("Task 'deploy'"), "{error}");
    assert!(error.contains("differ"), "{error}");
    assert_no_task_rows_and_no_repository_grant_rows(&database).await;
    database.finish().await;
}

#[tokio::test]
async fn task_repository_url_without_owner_and_name_stops_reconciliation_and_stores_nothing() {
    let Setup {
        database,
        db,
        project,
    } = setup(None).await;
    insert_shared_connection(&db, None, GitHostType::GitHub).await;

    let error = apply(&db, &state_tasks(&project, "https://github.com/infra"))
        .await
        .unwrap_err();

    assert!(error.contains("Task 'deploy'"), "{error}");
    assert!(error.contains("owner and repository"), "{error}");
    assert_no_task_rows_and_no_repository_grant_rows(&database).await;
    database.finish().await;
}
