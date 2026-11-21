/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use std::collections::{HashMap, HashSet};

use gradient_types::git_host::{
    base_url_of_git_host_connection, check_task_repository_url_can_use_git_host_connection,
    host_name_of_git_host_connection,
};
use gradient_types::*;
use gradient_util::http_base_url::ValidatedHttpBaseUrl;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DbErr, EntityTrait,
    QueryFilter, QueryOrder,
};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct GitRepositoryOwnerAndName {
    pub owner: String,
    pub name: String,
}

#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GitHostConnectionRepositoryGrantError {
    #[error("No connection to a Git host on the task")]
    NoConnection,
    #[error("The connection isn't available in the task's project.")]
    ConnectionOwnerDoesNotOwnTaskProject,
    #[error(
        "Gradient can't verify the host of the task repository because the connection has no base URL."
    )]
    MissingBaseUrl,
    #[error("The connection has a base URL that isn't an allowed http or https base URL: {0}")]
    ConnectionBaseUrlIsNotAllowed(String),
    #[error("{0}")]
    TaskRepositoryUrlOnOtherHostOrOwnerAndNameUnreadable(String),
    #[error("This connection has no grant for this repository in the task's project.")]
    RepositoryGrantMissingForTaskProject,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitHostConnectionRepositoryGrantCheck {
    pub repository: GitRepositoryOwnerAndName,
    pub needs_grant: bool,
}

pub fn check_git_host_connection_task_repository_access_without_grants(
    connection: &MGitHostConnection,
    project: &MProject,
    repository: &str,
) -> Result<GitHostConnectionRepositoryGrantCheck, GitHostConnectionRepositoryGrantError> {
    use GitHostConnectionRepositoryGrantError::*;
    let needs_grant = match (connection.project, connection.owner_team) {
        (Some(owner), _) if owner == project.id => false,
        (Some(_), _) => return Err(ConnectionOwnerDoesNotOwnTaskProject),
        (None, Some(team)) if project.owner_team == Some(team) => true,
        (None, Some(_)) => return Err(ConnectionOwnerDoesNotOwnTaskProject),
        (None, None) => true,
    };
    if let Some(base_url) = base_url_of_git_host_connection(connection) {
        base_url
            .parse::<ValidatedHttpBaseUrl>()
            .map_err(|e| ConnectionBaseUrlIsNotAllowed(e.to_string()))?;
    }
    if host_name_of_git_host_connection(connection).is_none() {
        return Err(MissingBaseUrl);
    }
    check_task_repository_url_can_use_git_host_connection(repository, connection)
        .map_err(TaskRepositoryUrlOnOtherHostOrOwnerAndNameUnreadable)?;
    let (owner, name) = parse_owner_repo(repository).ok_or_else(|| {
        TaskRepositoryUrlOnOtherHostOrOwnerAndNameUnreadable(format!(
            "Can't read owner and repository from {repository}"
        ))
    })?;
    Ok(GitHostConnectionRepositoryGrantCheck {
        repository: GitRepositoryOwnerAndName { owner, name },
        needs_grant,
    })
}

/// Grants don't apply to connections of the task's project. Connections of the instance, or of the
/// project's owning team, require a grant for the repository or a grant for the owner as a whole.
pub fn check_git_host_connection_task_repository_access_with_grants(
    connection: &MGitHostConnection,
    project: &MProject,
    repository: &str,
    grants: &[MGitHostConnectionRepositoryGrant],
) -> Result<GitRepositoryOwnerAndName, GitHostConnectionRepositoryGrantError> {
    let GitHostConnectionRepositoryGrantCheck {
        repository: GitRepositoryOwnerAndName { owner, name },
        needs_grant,
    } = check_git_host_connection_task_repository_access_without_grants(
        connection, project, repository,
    )?;
    let granted = grants.iter().any(|grant| {
        grant.git_host_connection == connection.id
            && grant.project == project.id
            && grant.repository_owner.eq_ignore_ascii_case(&owner)
            && grant
                .repository_name
                .as_ref()
                .is_none_or(|granted_name| granted_name.eq_ignore_ascii_case(&name))
    });
    if needs_grant && !granted {
        return Err(GitHostConnectionRepositoryGrantError::RepositoryGrantMissingForTaskProject);
    }
    Ok(GitRepositoryOwnerAndName { owner, name })
}

#[derive(Clone, Debug)]
pub struct GitHostConnectionRepositoryAccessProof {
    connection: MGitHostConnection,
    task: TaskId,
    repository: GitRepositoryOwnerAndName,
}

impl GitHostConnectionRepositoryAccessProof {
    pub fn connection(&self) -> &MGitHostConnection {
        &self.connection
    }

    pub fn task(&self) -> TaskId {
        self.task
    }

    pub fn repository(&self) -> &GitRepositoryOwnerAndName {
        &self.repository
    }
}

pub type TaskRepositoryConnectionGrantResult =
    Result<GitHostConnectionRepositoryAccessProof, GitHostConnectionRepositoryGrantError>;

pub async fn load_task_git_host_connection_project_and_grants_and_check_repository_access<
    C: ConnectionTrait,
>(
    db: &C,
    task: &MTask,
) -> Result<TaskRepositoryConnectionGrantResult, DbErr> {
    let Some(connection) = task.git_host_connection else {
        return Ok(Err(GitHostConnectionRepositoryGrantError::NoConnection));
    };
    load_task_project_and_grants_and_check_given_git_host_connection_for_task_repository_access(
        db, task, connection,
    )
    .await
}

pub async fn load_task_project_and_grants_and_check_given_git_host_connection_for_task_repository_access<
    C: ConnectionTrait,
>(
    db: &C,
    task: &MTask,
    connection: GitHostConnectionId,
) -> Result<TaskRepositoryConnectionGrantResult, DbErr> {
    let Some(connection) = EGitHostConnection::find_by_id(connection).one(db).await? else {
        return Ok(Err(GitHostConnectionRepositoryGrantError::NoConnection));
    };
    let project = EProject::find_by_id(task.project)
        .one(db)
        .await?
        .ok_or_else(|| DbErr::RecordNotFound(format!("Project {}", task.project)))?;
    let grants = of_connection_and_project(db, connection.id, project.id).await?;
    Ok(
        check_git_host_connection_task_repository_access_with_grants(
            &connection,
            &project,
            &task.repository,
            &grants,
        )
        .map(|repository| GitHostConnectionRepositoryAccessProof {
            connection,
            task: task.id,
            repository,
        }),
    )
}

pub async fn filter_task_ids_by_git_host_connection_repository_grants<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
    tasks: &[TaskId],
) -> Result<HashSet<TaskId>, DbErr> {
    if tasks.is_empty() {
        return Ok(HashSet::new());
    }
    let Some(connection) = EGitHostConnection::find_by_id(connection).one(db).await? else {
        return Ok(HashSet::new());
    };
    let tasks = ETask::find()
        .filter(CTask::Id.is_in(tasks.iter().copied()))
        .all(db)
        .await?;
    let projects: HashMap<ProjectId, MProject> = EProject::find()
        .filter(CProject::Id.is_in(tasks.iter().map(|task| task.project)))
        .all(db)
        .await?
        .into_iter()
        .map(|project| (project.id, project))
        .collect();
    let grants = of_connection(db, connection.id).await?;
    Ok(tasks
        .into_iter()
        .filter(|task| {
            let Some(project) = projects.get(&task.project) else {
                return false;
            };
            match check_git_host_connection_task_repository_access_with_grants(
                &connection,
                project,
                &task.repository,
                &grants,
            ) {
                Ok(_) => true,
                Err(refusal) => {
                    tracing::debug!(task = %task.id, connection = %connection.id, %refusal, "git host connection can't operate on the task repository");
                    false
                }
            }
        })
        .map(|task| task.id)
        .collect())
}

pub async fn of_connection_and_project<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
    project: ProjectId,
) -> Result<Vec<MGitHostConnectionRepositoryGrant>, DbErr> {
    EGitHostConnectionRepositoryGrant::find()
        .filter(CGitHostConnectionRepositoryGrant::GitHostConnection.eq(connection))
        .filter(CGitHostConnectionRepositoryGrant::Project.eq(project))
        .all(db)
        .await
}

pub async fn of_connection<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
) -> Result<Vec<MGitHostConnectionRepositoryGrant>, DbErr> {
    EGitHostConnectionRepositoryGrant::find()
        .filter(CGitHostConnectionRepositoryGrant::GitHostConnection.eq(connection))
        .order_by_asc(CGitHostConnectionRepositoryGrant::Project)
        .order_by_asc(CGitHostConnectionRepositoryGrant::RepositoryOwner)
        .order_by_asc(CGitHostConnectionRepositoryGrant::RepositoryName)
        .all(db)
        .await
}

pub async fn of_project<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MGitHostConnectionRepositoryGrant>, DbErr> {
    EGitHostConnectionRepositoryGrant::find()
        .filter(CGitHostConnectionRepositoryGrant::Project.eq(project))
        .order_by_asc(CGitHostConnectionRepositoryGrant::RepositoryOwner)
        .order_by_asc(CGitHostConnectionRepositoryGrant::RepositoryName)
        .all(db)
        .await
}

pub struct NewGitHostConnectionRepositoryGrant<'a> {
    pub connection: GitHostConnectionId,
    pub project: ProjectId,
    pub repository_owner: &'a str,
    pub repository_name: Option<&'a str>,
    pub source: RepositoryGrantSource,
    pub created_by: Option<UserId>,
}

pub async fn insert<C: ConnectionTrait>(
    db: &C,
    grant: NewGitHostConnectionRepositoryGrant<'_>,
) -> Result<MGitHostConnectionRepositoryGrant, DbErr> {
    AGitHostConnectionRepositoryGrant {
        id: Set(GitHostConnectionRepositoryGrantId::now_v7()),
        git_host_connection: Set(grant.connection),
        project: Set(grant.project),
        repository_owner: Set(grant.repository_owner.to_lowercase()),
        repository_name: Set(grant.repository_name.map(str::to_lowercase)),
        source: Set(grant.source),
        created_by: Set(grant.created_by),
        created_at: Set(chrono::Utc::now().naive_utc()),
    }
    .insert(db)
    .await
}

pub async fn of_connection_project_and_repository<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
    project: ProjectId,
    repository_owner: &str,
    repository_name: Option<&str>,
) -> Result<Option<MGitHostConnectionRepositoryGrant>, DbErr> {
    let name = match repository_name {
        Some(name) => CGitHostConnectionRepositoryGrant::RepositoryName.eq(name.to_lowercase()),
        None => CGitHostConnectionRepositoryGrant::RepositoryName.is_null(),
    };
    EGitHostConnectionRepositoryGrant::find()
        .filter(CGitHostConnectionRepositoryGrant::GitHostConnection.eq(connection))
        .filter(CGitHostConnectionRepositoryGrant::Project.eq(project))
        .filter(
            CGitHostConnectionRepositoryGrant::RepositoryOwner.eq(repository_owner.to_lowercase()),
        )
        .filter(name)
        .one(db)
        .await
}

pub async fn take_over_as_admin_grant<C: ConnectionTrait>(
    db: &C,
    grant: MGitHostConnectionRepositoryGrant,
    admin: UserId,
) -> Result<MGitHostConnectionRepositoryGrant, DbErr> {
    let mut taken_over: AGitHostConnectionRepositoryGrant = grant.into();
    taken_over.source = Set(RepositoryGrantSource::Admin);
    taken_over.created_by = Set(Some(admin));
    taken_over.update(db).await
}

pub async fn find_one_of_connection_by_id<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
    grant: GitHostConnectionRepositoryGrantId,
) -> Result<Option<MGitHostConnectionRepositoryGrant>, DbErr> {
    EGitHostConnectionRepositoryGrant::find_by_id(grant)
        .filter(CGitHostConnectionRepositoryGrant::GitHostConnection.eq(connection))
        .one(db)
        .await
}

pub async fn delete_of_connection_by_id<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
    grant: GitHostConnectionRepositoryGrantId,
) -> Result<bool, DbErr> {
    let result = EGitHostConnectionRepositoryGrant::delete_many()
        .filter(CGitHostConnectionRepositoryGrant::Id.eq(grant))
        .filter(CGitHostConnectionRepositoryGrant::GitHostConnection.eq(connection))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct StateFileRepositoryGrant {
    connection: GitHostConnectionId,
    project: ProjectId,
    repository: GitRepositoryOwnerAndName,
}

impl StateFileRepositoryGrant {
    pub fn new(
        connection: GitHostConnectionId,
        project: ProjectId,
        repository: &GitRepositoryOwnerAndName,
    ) -> Self {
        Self {
            connection,
            project,
            repository: GitRepositoryOwnerAndName {
                owner: repository.owner.to_lowercase(),
                name: repository.name.to_lowercase(),
            },
        }
    }

    fn is_exactly(&self, grant: &MGitHostConnectionRepositoryGrant) -> bool {
        grant.git_host_connection == self.connection
            && grant.project == self.project
            && grant.repository_owner == self.repository.owner
            && grant.repository_name.as_deref() == Some(self.repository.name.as_str())
    }
}

/// Exact "upgrade" grants become "state file" grants. The state file can stop implying such a
/// grant. The sync removes that grant. Admin grants stay unchanged. Exact admin grants already
/// count as satisfied.
pub async fn make_state_file_repository_grants_match_state_file<C: ConnectionTrait>(
    db: &C,
    implied: &HashSet<StateFileRepositoryGrant>,
) -> Result<(), DbErr> {
    let existing = EGitHostConnectionRepositoryGrant::find().all(db).await?;
    let no_longer_implied: Vec<&MGitHostConnectionRepositoryGrant> = existing
        .iter()
        .filter(|grant| grant.source == RepositoryGrantSource::StateFile)
        .filter(|grant| !implied.iter().any(|wanted| wanted.is_exactly(grant)))
        .collect();
    for grant in &no_longer_implied {
        tracing::info!(
            connection = %grant.git_host_connection,
            project = %grant.project,
            repository_owner = %grant.repository_owner,
            repository_name = grant.repository_name.as_deref().unwrap_or_default(),
            "Removed repository grants that are no longer implicit in the state file."
        );
    }
    if !no_longer_implied.is_empty() {
        EGitHostConnectionRepositoryGrant::delete_many()
            .filter(
                CGitHostConnectionRepositoryGrant::Id
                    .is_in(no_longer_implied.iter().map(|grant| grant.id)),
            )
            .exec(db)
            .await?;
    }
    for wanted in implied {
        match existing.iter().find(|grant| wanted.is_exactly(grant)) {
            Some(grant) if grant.source == RepositoryGrantSource::Upgrade => {
                let mut taken_over: AGitHostConnectionRepositoryGrant = grant.clone().into();
                taken_over.source = Set(RepositoryGrantSource::StateFile);
                taken_over.update(db).await?;
                tracing::info!(
                    connection = %wanted.connection,
                    project = %wanted.project,
                    repository_owner = %wanted.repository.owner,
                    repository_name = %wanted.repository.name,
                    "The state file took over the repository grants that the upgrade created."
                );
                continue;
            }
            Some(_) => continue,
            None => {}
        }
        tracing::info!(
            connection = %wanted.connection,
            project = %wanted.project,
            repository_owner = %wanted.repository.owner,
            repository_name = %wanted.repository.name,
            "Added a repository grant from the state file."
        );
        insert(
            db,
            NewGitHostConnectionRepositoryGrant {
                connection: wanted.connection,
                project: wanted.project,
                repository_owner: &wanted.repository.owner,
                repository_name: Some(&wanted.repository.name),
                source: RepositoryGrantSource::StateFile,
                created_by: None,
            },
        )
        .await?;
    }
    Ok(())
}

#[cfg(test)]
mod repository_grant_upgrade_migration_tests;

#[cfg(test)]
mod repository_grant_postgres_tests {
    use super::*;
    use gradient_entity::{git_host_connection, project, task, team, user};
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::ids::{team_id, user_id};
    use sea_orm::DatabaseConnection;

    const REPOSITORY: &str = "https://github.com/acme/infra";

    async fn create_fresh_test_database_with_one_user_and_one_team()
    -> (TestDatabase, DatabaseConnection) {
        let database = TestDatabase::create().await;
        let db = database.connection();
        crate::rows::insert::<user::Entity, _>(&db, gradient_fixtures::rows::user())
            .await
            .expect("user");
        crate::rows::insert::<team::Entity, _>(&db, gradient_fixtures::rows::team())
            .await
            .expect("team");
        (database, db)
    }

    async fn insert_project_row_with_optional_team_owner(
        db: &DatabaseConnection,
        owner_team: Option<TeamId>,
    ) -> MProject {
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

    async fn insert_git_host_connection_owned_by_project_team_or_instance(
        db: &DatabaseConnection,
        project: Option<ProjectId>,
        owner_team: Option<TeamId>,
    ) -> MGitHostConnection {
        let id = GitHostConnectionId::now_v7();
        crate::rows::insert::<git_host_connection::Entity, _>(
            db,
            git_host_connection::Model {
                id,
                project,
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

    async fn insert_task_with_repository_url_and_git_host_connection_in_project(
        db: &DatabaseConnection,
        project: &MProject,
        connection: &MGitHostConnection,
    ) -> MTask {
        let id = TaskId::now_v7();
        crate::rows::insert::<task::Entity, _>(
            db,
            task::Model {
                id,
                project: project.id,
                name: format!("task-{id}"),
                repository: REPOSITORY.into(),
                git_host_connection: Some(connection.id),
                ..gradient_fixtures::rows::task()
            },
        )
        .await
        .expect("task")
    }

    async fn insert_repository_grant_row_for_connection_project_owner_and_optional_repository_name(
        db: &DatabaseConnection,
        connection: &MGitHostConnection,
        project: &MProject,
    ) {
        insert(
            db,
            NewGitHostConnectionRepositoryGrant {
                connection: connection.id,
                project: project.id,
                repository_owner: "Acme",
                repository_name: Some("infra"),
                source: RepositoryGrantSource::Admin,
                created_by: Some(user_id()),
            },
        )
        .await
        .expect("grant");
    }

    async fn check(db: &DatabaseConnection, task: &MTask) -> TaskRepositoryConnectionGrantResult {
        load_task_git_host_connection_project_and_grants_and_check_repository_access(db, task)
            .await
            .expect("loader query")
    }

    #[tokio::test]
    async fn loader_allows_a_connection_of_the_task_project_without_a_grant_and_refuses_an_instance_connection_until_granted()
     {
        let (database, db) = create_fresh_test_database_with_one_user_and_one_team().await;
        let project = insert_project_row_with_optional_team_owner(&db, None).await;
        let own = insert_git_host_connection_owned_by_project_team_or_instance(
            &db,
            Some(project.id),
            None,
        )
        .await;
        let shared =
            insert_git_host_connection_owned_by_project_team_or_instance(&db, None, None).await;
        let task_with_own_connection =
            insert_task_with_repository_url_and_git_host_connection_in_project(&db, &project, &own)
                .await;
        let task_with_shared_connection =
            insert_task_with_repository_url_and_git_host_connection_in_project(
                &db, &project, &shared,
            )
            .await;

        let own_check = check(&db, &task_with_own_connection)
            .await
            .expect("own connection acts");
        assert_eq!(own_check.connection().id, own.id);
        assert_eq!(own_check.task(), task_with_own_connection.id);
        assert_eq!(own_check.repository().name, "infra");
        assert_eq!(
            check(&db, &task_with_shared_connection).await.err(),
            Some(GitHostConnectionRepositoryGrantError::RepositoryGrantMissingForTaskProject)
        );

        insert_repository_grant_row_for_connection_project_owner_and_optional_repository_name(
            &db, &shared, &project,
        )
        .await;
        let granted = check(&db, &task_with_shared_connection)
            .await
            .expect("granted instance connection acts");
        assert_eq!(granted.connection().id, shared.id);
        assert_eq!(granted.repository().owner, "acme");
        database.finish().await;
    }

    #[tokio::test]
    async fn loader_refuses_a_team_connection_for_a_task_of_another_team_even_with_a_grant() {
        let (database, db) = create_fresh_test_database_with_one_user_and_one_team().await;
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
        let project_of_other_team =
            insert_project_row_with_optional_team_owner(&db, Some(other_team)).await;
        let team_connection = insert_git_host_connection_owned_by_project_team_or_instance(
            &db,
            None,
            Some(team_id()),
        )
        .await;
        let task = insert_task_with_repository_url_and_git_host_connection_in_project(
            &db,
            &project_of_other_team,
            &team_connection,
        )
        .await;
        insert_repository_grant_row_for_connection_project_owner_and_optional_repository_name(
            &db,
            &team_connection,
            &project_of_other_team,
        )
        .await;

        assert_eq!(
            check(&db, &task).await.err(),
            Some(GitHostConnectionRepositoryGrantError::ConnectionOwnerDoesNotOwnTaskProject)
        );
        database.finish().await;
    }

    #[tokio::test]
    async fn batch_filter_keeps_only_the_tasks_whose_repository_the_connection_holds_a_grant_for() {
        let (database, db) = create_fresh_test_database_with_one_user_and_one_team().await;
        let granted_project = insert_project_row_with_optional_team_owner(&db, None).await;
        let other_project = insert_project_row_with_optional_team_owner(&db, None).await;
        let shared =
            insert_git_host_connection_owned_by_project_team_or_instance(&db, None, None).await;
        let granted_task = insert_task_with_repository_url_and_git_host_connection_in_project(
            &db,
            &granted_project,
            &shared,
        )
        .await;
        let ungranted_task = insert_task_with_repository_url_and_git_host_connection_in_project(
            &db,
            &other_project,
            &shared,
        )
        .await;
        insert_repository_grant_row_for_connection_project_owner_and_optional_repository_name(
            &db,
            &shared,
            &granted_project,
        )
        .await;

        let allowed = filter_task_ids_by_git_host_connection_repository_grants(
            &db,
            shared.id,
            &[granted_task.id, ungranted_task.id],
        )
        .await
        .expect("batch query");
        assert_eq!(allowed, HashSet::from([granted_task.id]));
        database.finish().await;
    }

    #[tokio::test]
    async fn test_repository_grant_lookup_and_delete_by_id_only_reach_grants_of_named_git_host_connection()
     {
        let (database, db) = create_fresh_test_database_with_one_user_and_one_team().await;
        let project = insert_project_row_with_optional_team_owner(&db, None).await;
        let shared =
            insert_git_host_connection_owned_by_project_team_or_instance(&db, None, None).await;
        let other =
            insert_git_host_connection_owned_by_project_team_or_instance(&db, None, None).await;
        insert_repository_grant_row_for_connection_project_owner_and_optional_repository_name(
            &db, &shared, &project,
        )
        .await;
        let grant = of_connection(&db, shared.id).await.expect("grants")[0].clone();

        let found_through_other = find_one_of_connection_by_id(&db, other.id, grant.id)
            .await
            .expect("lookup");
        let deleted_through_other = delete_of_connection_by_id(&db, other.id, grant.id)
            .await
            .expect("delete");
        let found = find_one_of_connection_by_id(&db, shared.id, grant.id)
            .await
            .expect("lookup");
        let deleted = delete_of_connection_by_id(&db, shared.id, grant.id)
            .await
            .expect("delete");

        assert_eq!(found_through_other, None);
        assert!(!deleted_through_other);
        assert_eq!(found, Some(grant));
        assert!(deleted);
        assert!(
            of_connection(&db, shared.id)
                .await
                .expect("grants")
                .is_empty()
        );
        database.finish().await;
    }

    #[tokio::test]
    async fn a_state_file_or_upgrade_grant_taken_over_by_an_admin_survives_the_next_state_apply() {
        let (database, db) = create_fresh_test_database_with_one_user_and_one_team().await;
        let project = insert_project_row_with_optional_team_owner(&db, None).await;
        let shared =
            insert_git_host_connection_owned_by_project_team_or_instance(&db, None, None).await;
        for (name, source) in [
            ("from-state-file", RepositoryGrantSource::StateFile),
            ("from-upgrade", RepositoryGrantSource::Upgrade),
        ] {
            insert(
                &db,
                NewGitHostConnectionRepositoryGrant {
                    connection: shared.id,
                    project: project.id,
                    repository_owner: "acme",
                    repository_name: Some(name),
                    source,
                    created_by: None,
                },
            )
            .await
            .expect("grant");
            let existing = of_connection_project_and_repository(
                &db,
                shared.id,
                project.id,
                "ACME",
                Some(&name.to_uppercase()),
            )
            .await
            .expect("lookup")
            .expect("exact grant");
            take_over_as_admin_grant(&db, existing, user_id())
                .await
                .expect("take over");
        }

        make_state_file_repository_grants_match_state_file(&db, &HashSet::new())
            .await
            .expect("state apply");

        let remaining = of_connection(&db, shared.id).await.expect("grants");
        assert_eq!(remaining.len(), 2);
        assert!(remaining.iter().all(|grant| {
            grant.source == RepositoryGrantSource::Admin && grant.created_by == Some(user_id())
        }));
        assert_eq!(
            of_connection_project_and_repository(&db, shared.id, project.id, "acme", None)
                .await
                .expect("lookup"),
            None
        );
        database.finish().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use GitHostConnectionRepositoryGrantError::*;

    fn project(owner_team: Option<TeamId>) -> MProject {
        MProject {
            id: ProjectId::now_v7(),
            owner_team,
            ..Default::default()
        }
    }

    fn connection(
        git_host_type: GitHostType,
        base_url: Option<&str>,
        project: Option<ProjectId>,
        owner_team: Option<TeamId>,
    ) -> MGitHostConnection {
        MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            git_host_type,
            base_url: base_url.map(str::to_owned),
            project,
            owner_team,
            ..Default::default()
        }
    }

    fn instance_github() -> MGitHostConnection {
        connection(GitHostType::GitHub, None, None, None)
    }

    fn grant(
        connection: &MGitHostConnection,
        project: &MProject,
        owner: &str,
        name: Option<&str>,
    ) -> MGitHostConnectionRepositoryGrant {
        MGitHostConnectionRepositoryGrant {
            id: GitHostConnectionRepositoryGrantId::now_v7(),
            git_host_connection: connection.id,
            project: project.id,
            repository_owner: owner.to_owned(),
            repository_name: name.map(str::to_owned),
            ..Default::default()
        }
    }

    fn owner_and_name(owner: &str, name: &str) -> GitRepositoryOwnerAndName {
        GitRepositoryOwnerAndName {
            owner: owner.to_owned(),
            name: name.to_owned(),
        }
    }

    #[test]
    fn a_connection_of_the_task_project_acts_without_a_grant() {
        let project = project(None);
        let own = connection(GitHostType::GitHub, None, Some(project.id), None);
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &own,
                &project,
                "https://github.com/acme/infra",
                &[],
            ),
            Ok(owner_and_name("acme", "infra"))
        );
    }

    #[test]
    fn a_connection_of_another_project_is_refused_even_with_a_grant() {
        let project = project(None);
        let foreign = connection(GitHostType::GitHub, None, Some(ProjectId::now_v7()), None);
        let grants = [grant(&foreign, &project, "acme", None)];
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &foreign,
                &project,
                "https://github.com/acme/infra",
                &grants,
            ),
            Err(ConnectionOwnerDoesNotOwnTaskProject)
        );
    }

    #[test]
    fn an_instance_connection_without_a_grant_is_refused() {
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &instance_github(),
                &project(None),
                "https://github.com/acme/infra",
                &[],
            ),
            Err(RepositoryGrantMissingForTaskProject)
        );
    }

    #[test]
    fn an_instance_connection_with_a_grant_for_the_repository_acts() {
        let project = project(None);
        let shared = instance_github();
        let grants = [grant(&shared, &project, "acme", Some("infra"))];
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &shared,
                &project,
                "https://github.com/acme/infra",
                &grants,
            ),
            Ok(owner_and_name("acme", "infra"))
        );
    }

    #[test]
    fn a_grant_for_another_repository_project_or_connection_does_not_count() {
        let other_project = project(None);
        let project = project(None);
        let shared = instance_github();
        let grants = [
            grant(&shared, &project, "acme", Some("other")),
            grant(&shared, &project, "evil", None),
            grant(&shared, &other_project, "acme", Some("infra")),
            grant(&instance_github(), &project, "acme", Some("infra")),
        ];
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &shared,
                &project,
                "https://github.com/acme/infra",
                &grants,
            ),
            Err(RepositoryGrantMissingForTaskProject)
        );
    }

    #[test]
    fn a_grant_for_every_repository_of_the_owner_covers_the_repository() {
        let project = project(None);
        let shared = instance_github();
        let grants = [grant(&shared, &project, "acme", None)];
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &shared,
                &project,
                "git@github.com:acme/infra.git",
                &grants,
            ),
            Ok(owner_and_name("acme", "infra"))
        );
    }

    #[test]
    fn owner_and_repository_compare_without_case() {
        let project = project(None);
        let shared = instance_github();
        let grants = [grant(&shared, &project, "acme", Some("infra"))];
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &shared,
                &project,
                "https://github.com/ACME/Infra",
                &grants,
            ),
            Ok(owner_and_name("ACME", "Infra"))
        );
    }

    #[test]
    fn a_gitlab_path_with_nested_groups_matches_only_as_a_whole() {
        let project = project(None);
        let shared = connection(GitHostType::GitLab, None, None, None);
        let repository = "https://gitlab.com/acme/platform/infra";
        let whole = [grant(&shared, &project, "acme", Some("platform/infra"))];
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &shared, &project, repository, &whole,
            ),
            Ok(owner_and_name("acme", "platform/infra"))
        );
        let prefix_only = [grant(&shared, &project, "acme", Some("platform"))];
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &shared,
                &project,
                repository,
                &prefix_only,
            ),
            Err(RepositoryGrantMissingForTaskProject)
        );
    }

    #[test]
    fn a_repository_on_another_host_is_refused_even_with_a_grant() {
        let project = project(None);
        let shared = connection(
            GitHostType::Gitea,
            Some("https://git.example.com"),
            None,
            None,
        );
        let grants = [grant(&shared, &project, "acme", None)];
        let refusal = check_git_host_connection_task_repository_access_with_grants(
            &shared,
            &project,
            "https://git.attacker.example/acme/infra",
            &grants,
        );
        assert!(
            matches!(&refusal, Err(TaskRepositoryUrlOnOtherHostOrOwnerAndNameUnreadable(reason)) if reason.contains("git.attacker.example")),
            "{refusal:?}"
        );
    }

    #[test]
    fn a_team_connection_acts_for_a_granted_project_of_the_team_only() {
        let team = TeamId::now_v7();
        let shared = connection(GitHostType::GitHub, None, None, Some(team));
        let inside = project(Some(team));
        let outside = project(Some(TeamId::now_v7()));
        let personal = project(None);
        let repository = "https://github.com/acme/infra";
        assert_eq!(
            check_git_host_connection_task_repository_access_with_grants(
                &shared,
                &inside,
                repository,
                &[grant(&shared, &inside, "acme", Some("infra"))],
            ),
            Ok(owner_and_name("acme", "infra"))
        );
        for project in [outside, personal] {
            assert_eq!(
                check_git_host_connection_task_repository_access_with_grants(
                    &shared,
                    &project,
                    repository,
                    &[grant(&shared, &project, "acme", Some("infra"))],
                ),
                Err(ConnectionOwnerDoesNotOwnTaskProject)
            );
        }
    }

    #[test]
    fn task_use_of_gitea_or_forgejo_connection_without_base_url_is_refused_for_instance_team_or_project()
     {
        let team = TeamId::now_v7();
        let project = project(Some(team));
        for git_host_type in [GitHostType::Gitea, GitHostType::Forgejo] {
            for (owner_project, owner_team) in
                [(None, None), (None, Some(team)), (Some(project.id), None)]
            {
                let without_base_url = connection(git_host_type, None, owner_project, owner_team);
                let grants = [grant(&without_base_url, &project, "acme", None)];
                assert_eq!(
                    check_git_host_connection_task_repository_access_with_grants(
                        &without_base_url,
                        &project,
                        "https://attacker.example/acme/infra",
                        &grants,
                    ),
                    Err(MissingBaseUrl),
                    "{git_host_type:?} {owner_project:?} {owner_team:?}"
                );
            }
        }
    }
}
