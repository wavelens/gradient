/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use sea_orm::{ConnectionTrait, DbErr, FromQueryResult, Value};

crate::sql! {
    SELECT_PROJECT_GIT_HTTPS_CREDENTIALS = "SELECT * FROM project_git_https_credential \
         WHERE project = $1 ORDER BY host",
        params = [ProjectId];
    SELECT_PROJECT_GIT_HTTPS_CREDENTIAL_FOR_HOST = "SELECT * FROM project_git_https_credential \
         WHERE project = $1 AND host = $2",
        params = [ProjectId, Text("github.com")];
    UPSERT_PROJECT_GIT_HTTPS_CREDENTIAL = "INSERT INTO project_git_https_credential \
         (id, project, host, username, password, updated_by, updated_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (project, host) DO UPDATE SET \
         username = EXCLUDED.username, password = EXCLUDED.password, \
         updated_by = EXCLUDED.updated_by, updated_at = EXCLUDED.updated_at \
         RETURNING *",
        params = [
            NewUuid,
            ProjectId,
            Text("github.com"),
            Text("x-access-token"),
            Text("encrypted"),
            UserId,
            Now,
        ];
    DELETE_PROJECT_GIT_HTTPS_CREDENTIAL_BY_ID = "DELETE FROM project_git_https_credential \
         WHERE id = $1 AND project = $2",
        params = [NewUuid, ProjectId];
}

pub async fn list<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MProjectGitHttpsCredential>, DbErr> {
    MProjectGitHttpsCredential::find_by_statement(
        SELECT_PROJECT_GIT_HTTPS_CREDENTIALS.bind([project_value(project)]),
    )
    .all(db)
    .await
}

pub async fn one_for_project_and_host<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    host: &str,
) -> Result<Option<MProjectGitHttpsCredential>, DbErr> {
    MProjectGitHttpsCredential::find_by_statement(
        SELECT_PROJECT_GIT_HTTPS_CREDENTIAL_FOR_HOST
            .bind([project_value(project), Value::String(Some(host.to_owned()))]),
    )
    .one(db)
    .await
}

pub async fn upsert<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    host: &str,
    username: &str,
    encrypted_password: String,
    updated_by: Option<UserId>,
) -> Result<MProjectGitHttpsCredential, DbErr> {
    MProjectGitHttpsCredential::find_by_statement(UPSERT_PROJECT_GIT_HTTPS_CREDENTIAL.bind([
        Value::Uuid(Some(ProjectGitHttpsCredentialId::now_v7().into_inner())),
        project_value(project),
        Value::String(Some(host.to_owned())),
        Value::String(Some(username.to_owned())),
        Value::String(Some(encrypted_password)),
        Value::Uuid(updated_by.map(UserId::into_inner)),
        Value::ChronoDateTime(Some(gradient_types::now())),
    ]))
    .one(db)
    .await?
    .ok_or_else(|| DbErr::RecordNotInserted)
}

pub async fn delete<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    id: ProjectGitHttpsCredentialId,
) -> Result<bool, DbErr> {
    let result = db
        .execute_raw(
            DELETE_PROJECT_GIT_HTTPS_CREDENTIAL_BY_ID
                .bind([Value::Uuid(Some(id.into_inner())), project_value(project)]),
        )
        .await?;
    Ok(result.rows_affected() > 0)
}

fn project_value(project: ProjectId) -> Value {
    Value::Uuid(Some(project.into_inner()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::database::TestDatabase;
    use gradient_fixtures::rows;
    use sea_orm::EntityTrait;

    async fn database_with_project() -> (TestDatabase, ProjectId) {
        let db = TestDatabase::create().await;
        let conn = db.connection();
        crate::rows::insert::<EUser, _>(&conn, rows::user())
            .await
            .expect("the owner is written");
        let project = crate::rows::insert::<EProject, _>(&conn, rows::project())
            .await
            .expect("the project is written");
        (db, project.id)
    }

    #[tokio::test]
    async fn an_upsert_for_the_same_project_and_host_replaces_the_credential() {
        let (db, project) = database_with_project().await;
        let conn = db.connection();
        let first = upsert(&conn, project, "github.com", "ci", "old".into(), None)
            .await
            .expect("the first credential is written");
        let second = upsert(
            &conn,
            project,
            "github.com",
            "x-access-token",
            "new".into(),
            Some(rows::user().id),
        )
        .await
        .expect("the second credential replaces the first");
        upsert(
            &conn,
            project,
            "git.example:8443",
            "ci",
            "other".into(),
            None,
        )
        .await
        .expect("a credential for another host is written");

        assert_eq!(second.id, first.id);
        let stored = one_for_project_and_host(&conn, project, "github.com")
            .await
            .expect("the credential reads")
            .expect("the credential exists");
        assert_eq!(
            (stored.username.as_str(), stored.password.as_str()),
            ("x-access-token", "new")
        );
        assert_eq!(stored.updated_by, Some(rows::user().id));
        let hosts: Vec<String> = list(&conn, project)
            .await
            .expect("the credentials list")
            .into_iter()
            .map(|credential| credential.host)
            .collect();
        assert_eq!(hosts, ["git.example:8443", "github.com"]);
        db.finish().await;
    }

    #[tokio::test]
    async fn a_credential_is_deleted_only_through_its_own_project() {
        let (db, project) = database_with_project().await;
        let conn = db.connection();
        let credential = upsert(&conn, project, "github.com", "ci", "secret".into(), None)
            .await
            .expect("the credential is written");

        assert!(
            !delete(&conn, ProjectId::now_v7(), credential.id)
                .await
                .expect("the foreign delete runs")
        );
        assert!(
            delete(&conn, project, credential.id)
                .await
                .expect("the delete runs")
        );
        assert!(
            one_for_project_and_host(&conn, project, "github.com")
                .await
                .expect("the credential reads")
                .is_none()
        );
        db.finish().await;
    }

    #[tokio::test]
    async fn deleting_the_project_removes_its_credentials() {
        let (db, project) = database_with_project().await;
        let conn = db.connection();
        upsert(&conn, project, "github.com", "ci", "secret".into(), None)
            .await
            .expect("the credential is written");

        EProject::delete_by_id(project)
            .exec(&conn)
            .await
            .expect("the project is deleted");

        assert!(
            list(&conn, project)
                .await
                .expect("the credentials list")
                .is_empty()
        );
        db.finish().await;
    }
}
