/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_entity::task_commit_status_post_attempt as post_attempt;
use gradient_types::*;
use sea_orm::sea_query::{Condition, Query};
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
        crate::rows::insert::<gradient_entity::user::Entity, _>(
            &connection,
            gradient_fixtures::rows::user(),
        )
        .await
        .expect("the old schema can save users");
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
}
