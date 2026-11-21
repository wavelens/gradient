/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::task_commit_status_post_attempt as post_attempt;
use gradient_types::*;
use sea_orm::sea_query::{Condition, Expr, ExprTrait, Query};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, IntoActiveModel,
    QueryFilter, QueryOrder, QuerySelect,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitHostConnectionOwnerByDatabaseId {
    Instance,
    Team(TeamId),
    Project(ProjectId),
}

impl GitHostConnectionOwnerByDatabaseId {
    pub fn of(connection: &MGitHostConnection) -> Self {
        match (connection.project, connection.owner_team) {
            (Some(project), _) => Self::Project(project),
            (None, Some(team)) => Self::Team(team),
            (None, None) => Self::Instance,
        }
    }

    fn condition(self) -> Condition {
        match self {
            Self::Instance => Condition::all()
                .add(CGitHostConnection::Project.is_null())
                .add(CGitHostConnection::OwnerTeam.is_null()),
            Self::Team(team) => Condition::all().add(CGitHostConnection::OwnerTeam.eq(team)),
            Self::Project(project) => Condition::all().add(CGitHostConnection::Project.eq(project)),
        }
    }
}

pub async fn of_owner<C: ConnectionTrait>(
    db: &C,
    owner: GitHostConnectionOwnerByDatabaseId,
) -> Result<Vec<MGitHostConnection>, DbErr> {
    EGitHostConnection::find()
        .filter(owner.condition())
        .order_by_asc(CGitHostConnection::Name)
        .all(db)
        .await
}

pub async fn list_git_host_connections_available_to_project<C: ConnectionTrait>(
    db: &C,
    project: &MProject,
) -> Result<Vec<MGitHostConnection>, DbErr> {
    let mut available = Condition::any()
        .add(GitHostConnectionOwnerByDatabaseId::Project(project.id).condition())
        .add(GitHostConnectionOwnerByDatabaseId::Instance.condition());
    if let Some(team) = project.owner_team {
        available = available.add(GitHostConnectionOwnerByDatabaseId::Team(team).condition());
    }
    EGitHostConnection::find()
        .filter(available)
        .order_by_asc(CGitHostConnection::Name)
        .all(db)
        .await
}

pub async fn by_name<C: ConnectionTrait>(
    db: &C,
    owner: GitHostConnectionOwnerByDatabaseId,
    name: &str,
) -> Result<Option<MGitHostConnection>, DbErr> {
    EGitHostConnection::find()
        .filter(owner.condition())
        .filter(CGitHostConnection::Name.eq(name))
        .one(db)
        .await
}

pub async fn other_by_name<C: ConnectionTrait>(
    db: &C,
    owner: GitHostConnectionOwnerByDatabaseId,
    name: &str,
    except: GitHostConnectionId,
) -> Result<Option<MGitHostConnection>, DbErr> {
    EGitHostConnection::find()
        .filter(owner.condition())
        .filter(CGitHostConnection::Name.eq(name))
        .filter(CGitHostConnection::Id.ne(except))
        .one(db)
        .await
}

pub async fn by_id_of_owner<C: ConnectionTrait>(
    db: &C,
    owner: GitHostConnectionOwnerByDatabaseId,
    id: GitHostConnectionId,
) -> Result<Option<MGitHostConnection>, DbErr> {
    EGitHostConnection::find_by_id(id)
        .filter(owner.condition())
        .one(db)
        .await
}

pub async fn by_github_installation_id<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
) -> Result<Option<MGitHostConnection>, DbErr> {
    EGitHostConnection::find()
        .filter(CGitHostConnection::GithubInstallationId.eq(installation_id))
        .one(db)
        .await
}

pub async fn delete_by_github_installation_id<C: ConnectionTrait>(
    db: &C,
    installation_id: i64,
) -> Result<(), DbErr> {
    EGitHostConnection::delete_many()
        .filter(CGitHostConnection::GithubInstallationId.eq(installation_id))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn write_all_columns_of_git_host_connection<C: ConnectionTrait>(
    db: &C,
    connection: MGitHostConnection,
) -> Result<MGitHostConnection, DbErr> {
    crate::rows::update::<EGitHostConnection, _>(db, connection.into_active_model().reset_all())
        .await
}

pub async fn count_refused_signature<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
) -> Result<(), DbErr> {
    EGitHostConnection::update_many()
        .col_expr(
            CGitHostConnection::RefusedSignatureCount,
            Expr::col(CGitHostConnection::RefusedSignatureCount).add(1),
        )
        .col_expr(
            CGitHostConnection::LastRefusedSignatureAt,
            Expr::value(gradient_types::now()),
        )
        .filter(CGitHostConnection::Id.eq(connection))
        .exec(db)
        .await?;
    Ok(())
}

const POST_ATTEMPTS_KEPT_PER_TASK: u64 = 50;

pub async fn record_commit_status_post_attempt<C: ConnectionTrait>(
    db: &C,
    attempt: post_attempt::Model,
) -> Result<(), DbErr> {
    let task = attempt.task;
    crate::rows::insert::<post_attempt::Entity, _>(db, attempt).await?;
    let kept = Query::select()
        .column(post_attempt::Column::Id)
        .from(post_attempt::Entity)
        .and_where(post_attempt::Column::Task.eq(task))
        .order_by(post_attempt::Column::DeliveredAt, sea_orm::Order::Desc)
        .limit(POST_ATTEMPTS_KEPT_PER_TASK)
        .to_owned();
    post_attempt::Entity::delete_many()
        .filter(post_attempt::Column::Task.eq(task))
        .filter(post_attempt::Column::Id.not_in_subquery(kept))
        .exec(db)
        .await?;
    Ok(())
}

pub async fn newest_commit_status_post_attempt_per_connection<C: ConnectionTrait>(
    db: &C,
    connections: &[GitHostConnectionId],
    task_projects: &[ProjectId],
) -> Result<Vec<post_attempt::Model>, DbErr> {
    if connections.is_empty() || task_projects.is_empty() {
        return Ok(Vec::new());
    }
    let tasks_of_projects = Query::select()
        .column(CTask::Id)
        .from(gradient_entity::task::Entity)
        .and_where(CTask::Project.is_in(task_projects.iter().copied()))
        .to_owned();
    post_attempt::Entity::find()
        .distinct_on([post_attempt::Column::GitHostConnection])
        .filter(post_attempt::Column::GitHostConnection.is_in(connections.iter().copied()))
        .filter(post_attempt::Column::Task.in_subquery(tasks_of_projects))
        .order_by_asc(post_attempt::Column::GitHostConnection)
        .order_by_desc(post_attempt::Column::DeliveredAt)
        .all(db)
        .await
}

pub async fn task_counts_by_connection_and_project<C: ConnectionTrait>(
    db: &C,
    connections: &[GitHostConnectionId],
) -> Result<Vec<(GitHostConnectionId, ProjectId, i64)>, DbErr> {
    if connections.is_empty() {
        return Ok(Vec::new());
    }
    ETask::find()
        .select_only()
        .column(CTask::GitHostConnection)
        .column(CTask::Project)
        .column_as(CTask::Id.count(), "task_count")
        .filter(CTask::GitHostConnection.is_in(connections.iter().copied()))
        .group_by(CTask::GitHostConnection)
        .group_by(CTask::Project)
        .into_tuple()
        .all(db)
        .await
}

pub async fn repositories_of_project_tasks_by_connection<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    connections: &[GitHostConnectionId],
) -> Result<Vec<(GitHostConnectionId, String)>, DbErr> {
    if connections.is_empty() {
        return Ok(Vec::new());
    }
    ETask::find()
        .select_only()
        .column(CTask::GitHostConnection)
        .column(CTask::Repository)
        .filter(CTask::Project.eq(project))
        .filter(CTask::GitHostConnection.is_in(connections.iter().copied()))
        .into_tuple()
        .all(db)
        .await
}

pub async fn tasks_using_connection<C: ConnectionTrait>(
    db: &C,
    connection: GitHostConnectionId,
) -> Result<Vec<MTask>, DbErr> {
    ETask::find()
        .filter(CTask::GitHostConnection.eq(connection))
        .order_by_asc(CTask::Name)
        .all(db)
        .await
}

pub async fn link_tasks_to_connection<C: ConnectionTrait>(
    db: &C,
    tasks: &[TaskId],
    connection: GitHostConnectionId,
    report_status: bool,
) -> Result<(), DbErr> {
    if tasks.is_empty() {
        return Ok(());
    }
    ETask::update_many()
        .col_expr(CTask::GitHostConnection, Expr::value(connection))
        .col_expr(CTask::ReportStatus, Expr::value(report_status))
        .filter(CTask::Id.is_in(tasks.iter().copied()))
        .filter(CTask::GitHostConnection.is_null())
        .exec(db)
        .await?;
    Ok(())
}

pub async fn commit_status_post_attempts_of_task<C: ConnectionTrait>(
    db: &C,
    task: TaskId,
    limit: u64,
) -> Result<Vec<post_attempt::Model>, DbErr> {
    post_attempt::Entity::find()
        .filter(post_attempt::Column::Task.eq(task))
        .order_by_desc(post_attempt::Column::DeliveredAt)
        .limit(limit)
        .all(db)
        .await
}

#[cfg(test)]
mod tests {
    use gradient_fixtures::ids::{project_id, task_id, user_id};
    use sea_orm_migration::MigratorTrait;

    const SELECT_GIT_HOST_CONNECTIONS_AS_TEXT_LINES: &str = "SELECT id::text || ' ' || receives_events || ' ' || \
         reports_status || ' ' || coalesce(base_url, '') || ' ' || coalesce(access_token, '') \
         FROM git_host_connection";

    #[tokio::test]
    async fn m20261208_migration_converts_integration_pair_to_git_host_connection_and_status_report_action_to_task_fields_and_down_restores_both()
     {
        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let migrations = gradient_migration::Migrator::migrations();
        let migrations_to_revert_to_schema_before_git_host_connection_migration = u32::try_from(
            migrations.len()
                - migrations
                    .iter()
                    .position(|m| m.name().starts_with("m20261208_000000_"))
                    .expect("register a migration from integrations to git host connections"),
        )
        .expect("the count fits in u32");
        gradient_migration::Migrator::down(
            &connection,
            Some(migrations_to_revert_to_schema_before_git_host_connection_migration),
        )
        .await
        .expect("migration after reverting the integration table");
        let inbound_integration_id = uuid::Uuid::now_v7();
        let outbound_integration_id_before_migration = uuid::Uuid::now_v7();
        db.execute(&format!(
            "INSERT INTO \"user\" (id, username, name, email, last_login_at, created_at) VALUES \
             ('{user}', 'testuser', 'Test User', 'test@example.com', now(), now())",
            user = user_id(),
        ))
        .await;
        crate::rows::insert::<gradient_entity::project::Entity, _>(
            &connection,
            gradient_fixtures::rows::project(),
        )
        .await
        .expect("the old schema stores the project");
        db.execute(&format!(
            "INSERT INTO task (id, project, name, active, display_name, description, repository, \
             wildcard, last_check_at, force_evaluation, created_by, created_at) VALUES \
             ('{task}', '{project}', 'app', true, 'App', '', 'https://gitea.example.com/acme/app', \
             '*', now(), false, '{user}', now()); \
             INSERT INTO integration (id, project, name, kind, git_host_type, secret, endpoint_url, \
             access_token, created_by, created_at) VALUES \
             ('{inbound_integration_id}', '{project}', 'gitea', 0, 0, 'secret', NULL, NULL, '{user}', now()), \
             ('{outbound_integration_id_before_migration}', '{project}', 'gitea', 1, 0, NULL, \
             'https://gitea.example.com', 'token', '{user}', now()); \
             INSERT INTO task_action (id, task, name, action_type, config, events, created_by) VALUES \
             (gen_random_uuid(), '{task}', 'status', 2, \
             '{{\"type\":\"git_host_status_report\",\"integration_id\":\"{outbound_integration_id_before_migration}\"}}', \
             '[]', '{user}');",
            task = task_id(),
            project = project_id(),
            user = user_id(),
        ))
        .await;

        gradient_migration::Migrator::up(&connection, None)
            .await
            .expect("One connection per integration pair after migration.");
        let connections = db.texts(SELECT_GIT_HOST_CONNECTIONS_AS_TEXT_LINES).await;
        let task_fields = db
            .texts(
                "SELECT coalesce(git_host_connection::text, '') || ' ' || report_status FROM task",
            )
            .await;
        let status_actions_after_up = db
            .texts("SELECT id::text FROM task_action WHERE action_type = 2")
            .await;
        gradient_migration::Migrator::down(
            &connection,
            Some(migrations_to_revert_to_schema_before_git_host_connection_migration),
        )
        .await
        .expect("migration restored integration and status report action");
        let integration_kinds = db
            .texts("SELECT kind::text FROM integration ORDER BY kind")
            .await;
        let status_actions_after_down = db
            .texts("SELECT config->>'type' FROM task_action WHERE action_type = 2")
            .await;
        gradient_migration::Migrator::up(&connection, None)
            .await
            .expect("migration rerun on restored integrations");
        let connections_after_second_up = db.texts(SELECT_GIT_HOST_CONNECTIONS_AS_TEXT_LINES).await;

        assert_eq!(
            connections,
            vec![format!(
                "{inbound_integration_id} true true https://gitea.example.com token"
            )]
        );
        assert_eq!(task_fields, vec![format!("{inbound_integration_id} true")]);
        assert!(status_actions_after_up.is_empty());
        assert_eq!(integration_kinds, vec!["0", "1"]);
        assert_eq!(status_actions_after_down, vec!["git_host_status_report"]);
        assert_eq!(connections_after_second_up, connections);
        db.finish().await;
    }

    #[tokio::test]
    async fn test_base_url_migration_backfills_gitlab_keeps_existing_gitea_and_forgejo_and_rejects_new_rows_without_base_url()
     {
        use gradient_entity::git_host_connection::{
            Entity as EGitHostConnection, GitHostType, Model as MGitHostConnection,
        };
        use gradient_entity::ids::GitHostConnectionId;
        use sea_orm_migration::SchemaManager;

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let migration = gradient_migration::Migrator::migrations()
            .into_iter()
            .find(|m| m.name().starts_with("m20261228_000000_"))
            .expect("register the migration that requires a base URL of git host connections");
        migration
            .down(&SchemaManager::new(&connection))
            .await
            .expect("the migration reverts");
        crate::rows::insert::<gradient_entity::user::Entity, _>(
            &connection,
            gradient_fixtures::rows::user(),
        )
        .await
        .expect("insert user");
        let row = |name: &str, git_host_type| MGitHostConnection {
            id: GitHostConnectionId::now_v7(),
            name: name.into(),
            display_name: name.into(),
            git_host_type,
            created_by: user_id(),
            created_at: chrono::Utc::now().naive_utc(),
            ..Default::default()
        };
        let mut legacy_ids = Vec::new();
        for (name, git_host_type) in [
            ("gitlab", GitHostType::GitLab),
            ("gitea", GitHostType::Gitea),
            ("forgejo", GitHostType::Forgejo),
            ("github", GitHostType::GitHub),
        ] {
            let stored =
                crate::rows::insert::<EGitHostConnection, _>(&connection, row(name, git_host_type))
                    .await
                    .expect("the old schema stores a connection without base URL");
            legacy_ids.push(stored.id);
        }

        migration
            .up(&SchemaManager::new(&connection))
            .await
            .expect("the migration runs");

        let mut base_urls = Vec::new();
        for id in legacy_ids {
            let stored = db
                .find::<EGitHostConnection>(id)
                .await
                .expect("row remains");
            base_urls.push(stored.base_url);
        }
        assert_eq!(
            base_urls,
            [Some("https://gitlab.com".to_owned()), None, None, None]
        );
        for (name, git_host_type) in [
            ("new-gitea", GitHostType::Gitea),
            ("new-forgejo", GitHostType::Forgejo),
            ("new-gitlab", GitHostType::GitLab),
        ] {
            let refused =
                crate::rows::insert::<EGitHostConnection, _>(&connection, row(name, git_host_type))
                    .await;
            assert!(refused.is_err(), "{name} without base URL was stored");
        }
        crate::rows::insert::<EGitHostConnection, _>(
            &connection,
            row("new-github", GitHostType::GitHub),
        )
        .await
        .expect("a GitHub connection needs no base URL");
        db.finish().await;
    }

    #[tokio::test]
    async fn m20261229_grants_shared_git_host_connections_to_task_repositories_in_owner_team_projects_and_detaches_tasks_without_owner_repo_path()
     {
        use gradient_entity::ids::{GitHostConnectionId, ProjectId, TaskId};
        use gradient_entity::{git_host_connection, project, task, team, user};

        let db = gradient_fixtures::database::TestDatabase::create().await;
        let connection = db.connection();
        let migrations = gradient_migration::Migrator::migrations();
        let migrations_to_revert_to_schema_before_repository_grants = u32::try_from(
            migrations.len()
                - migrations
                    .iter()
                    .position(|m| m.name().starts_with("m20261229_000000_"))
                    .expect("register the migration that creates repository grants"),
        )
        .expect("the count fits in u32");
        gradient_migration::Migrator::down(
            &connection,
            Some(migrations_to_revert_to_schema_before_repository_grants),
        )
        .await
        .expect("schema before repository grants");

        let team_project = project_id();
        let project_outside_team = ProjectId::now_v7();
        let instance_connection = GitHostConnectionId::now_v7();
        let team_connection = GitHostConnectionId::now_v7();
        let unparsable_task = TaskId::now_v7();
        let task_outside_team = TaskId::now_v7();
        crate::rows::insert::<user::Entity, _>(&connection, gradient_fixtures::rows::user())
            .await
            .expect("user");
        crate::rows::insert::<team::Entity, _>(&connection, gradient_fixtures::rows::team())
            .await
            .expect("team");
        for (id, name, owner_team) in [
            (
                team_project,
                "in-team",
                Some(gradient_fixtures::ids::team_id()),
            ),
            (project_outside_team, "outside-team", None),
        ] {
            crate::rows::insert::<project::Entity, _>(
                &connection,
                project::Model {
                    id,
                    name: name.into(),
                    owner_team,
                    ..gradient_fixtures::rows::project()
                },
            )
            .await
            .expect("project");
        }
        for (id, name, owner_team) in [
            (instance_connection, "instance", None),
            (
                team_connection,
                "team",
                Some(gradient_fixtures::ids::team_id()),
            ),
        ] {
            crate::rows::insert::<git_host_connection::Entity, _>(
                &connection,
                git_host_connection::Model {
                    id,
                    owner_team,
                    name: name.into(),
                    display_name: name.into(),
                    created_by: user_id(),
                    created_at: gradient_fixtures::test_date(),
                    base_url: Some("https://gitea.example.com".into()),
                    ..Default::default()
                },
            )
            .await
            .expect("connection");
        }
        for (id, project, name, repository, git_host_connection) in [
            (
                task_id(),
                team_project,
                "app",
                "https://github.com/Acme/App.git",
                Some(instance_connection),
            ),
            (
                unparsable_task,
                team_project,
                "no-repo-path",
                "https://github.com/acme",
                Some(instance_connection),
            ),
            (
                TaskId::now_v7(),
                team_project,
                "nested",
                "https://gitlab.example.com/group/sub/repo",
                Some(team_connection),
            ),
            (
                task_outside_team,
                project_outside_team,
                "outside",
                "https://gitlab.example.com/other/repo",
                Some(team_connection),
            ),
            (
                TaskId::now_v7(),
                team_project,
                "pull-requests",
                "git@github.com:acme/lib.git",
                None,
            ),
        ] {
            crate::rows::insert::<task::Entity, _>(
                &connection,
                task::Model {
                    id,
                    project,
                    name: name.into(),
                    repository: repository.into(),
                    git_host_connection,
                    report_status: git_host_connection.is_some(),
                    ..gradient_fixtures::rows::task()
                },
            )
            .await
            .expect("task");
        }
        db.execute(&format!(
            "INSERT INTO task_action (id, task, name, action_type, config, events, active, created_by) \
             SELECT gen_random_uuid(), id, 'open pr', 3, \
             '{{\"type\":\"open_pr\",\"git_host_connection_id\":\"{instance_connection}\"}}', '[]', true, \
             '{user}' FROM task WHERE name IN ('pull-requests', 'no-repo-path');",
            user = user_id(),
        ))
        .await;

        gradient_migration::Migrator::up(&connection, None)
            .await
            .expect("repository grants after migration");
        let grants = db
            .texts(
                "SELECT c.name || ' ' || p.name || ' ' || g.repository_owner || ' ' || \
                 coalesce(g.repository_name, '*') || ' ' || g.source || ' ' || \
                 coalesce(g.created_by::text, 'nobody') \
                 FROM git_host_connection_repository_grant g \
                 JOIN git_host_connection c ON c.id = g.git_host_connection \
                 JOIN project p ON p.id = g.project ORDER BY 1",
            )
            .await;
        let task_fields = db
            .texts(&format!(
                "SELECT name || ' ' || coalesce(git_host_connection::text, 'none') || ' ' || \
                 report_status FROM task WHERE id IN ('{unparsable_task}', '{task_outside_team}') \
                 ORDER BY name"
            ))
            .await;

        assert_eq!(
            grants,
            vec![
                "instance in-team acme app 1 nobody",
                "instance in-team acme lib 1 nobody",
                "team in-team group sub/repo 1 nobody",
            ]
        );
        assert_eq!(
            task_fields,
            vec![
                "no-repo-path none false".to_owned(),
                format!("outside {team_connection} true"),
            ]
        );
        db.finish().await;
    }
}
