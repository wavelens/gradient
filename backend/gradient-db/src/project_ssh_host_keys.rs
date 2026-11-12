/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

use gradient_types::*;
use gradient_wire::types::{GitServerSshHostKey, GitSshHostKeyObservation};
use sea_orm::sea_query::{Expr, OnConflict};
use sea_orm::{
    ActiveValue::Set, ColumnTrait, ConnectionTrait, DbErr, EntityTrait, QueryFilter, QueryOrder,
};

pub async fn list<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<MProjectSshHostKey>, DbErr> {
    EProjectSshHostKey::find()
        .filter(CProjectSshHostKey::Project.eq(project))
        .order_by_asc(CProjectSshHostKey::Host)
        .order_by_asc(CProjectSshHostKey::Port)
        .all(db)
        .await
}

pub async fn one<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    id: ProjectSshHostKeyId,
) -> Result<Option<MProjectSshHostKey>, DbErr> {
    EProjectSshHostKey::find_by_id(id)
        .filter(CProjectSshHostKey::Project.eq(project))
        .one(db)
        .await
}

pub async fn list_project_ssh_host_keys_for_git_connections<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
) -> Result<Vec<GitServerSshHostKey>, DbErr> {
    Ok(list(db, project)
        .await?
        .into_iter()
        .map(|row| GitServerSshHostKey {
            host: row.host,
            port: u16::try_from(row.port).unwrap_or_default(),
            public_key: row.public_key,
        })
        .collect())
}

pub async fn upsert<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    host: &str,
    port: u16,
    public_key: &str,
) -> Result<MProjectSshHostKey, DbErr> {
    EProjectSshHostKey::insert(new_row(project, host, port, public_key))
        .on_conflict(
            OnConflict::columns(host_columns())
                .update_columns([
                    CProjectSshHostKey::PublicKey,
                    CProjectSshHostKey::LearnedAt,
                    CProjectSshHostKey::ChangedAt,
                ])
                .to_owned(),
        )
        .exec_with_returning(db)
        .await
}

pub async fn delete<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    id: ProjectSshHostKeyId,
) -> Result<bool, DbErr> {
    let result = EProjectSshHostKey::delete_many()
        .filter(CProjectSshHostKey::Project.eq(project))
        .filter(CProjectSshHostKey::Id.eq(id))
        .exec(db)
        .await?;
    Ok(result.rows_affected > 0)
}

pub async fn apply_project_ssh_host_key_observations<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    observations: &[GitSshHostKeyObservation],
) -> Result<(), DbErr> {
    for observation in observations {
        match observation {
            GitSshHostKeyObservation::UnknownHostKeyAcceptedForSaving { key } => {
                save_learned_key(db, project, key).await?
            }
            GitSshHostKeyObservation::HostKeyMismatchConnectionRefused { host, port } => {
                mark_changed(db, project, host, *port).await?
            }
        }
    }
    Ok(())
}

async fn save_learned_key<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    key: &GitServerSshHostKey,
) -> Result<(), DbErr> {
    let inserted =
        EProjectSshHostKey::insert(new_row(project, &key.host, key.port, &key.public_key))
            .on_conflict(OnConflict::columns(host_columns()).do_nothing().to_owned())
            .exec_without_returning(db)
            .await?;
    if inserted > 0 {
        return Ok(());
    }
    let saved = EProjectSshHostKey::find()
        .filter(CProjectSshHostKey::Project.eq(project))
        .filter(CProjectSshHostKey::Host.eq(key.host.to_lowercase()))
        .filter(CProjectSshHostKey::Port.eq(i32::from(key.port)))
        .one(db)
        .await?;
    if saved.is_some_and(|row| row.public_key != key.public_key) {
        mark_changed(db, project, &key.host, key.port).await?;
    }
    Ok(())
}

async fn mark_changed<C: ConnectionTrait>(
    db: &C,
    project: ProjectId,
    host: &str,
    port: u16,
) -> Result<(), DbErr> {
    EProjectSshHostKey::update_many()
        .col_expr(
            CProjectSshHostKey::ChangedAt,
            Expr::value(gradient_types::now()),
        )
        .filter(CProjectSshHostKey::Project.eq(project))
        .filter(CProjectSshHostKey::Host.eq(host.to_lowercase()))
        .filter(CProjectSshHostKey::Port.eq(i32::from(port)))
        .exec(db)
        .await?;
    Ok(())
}

fn new_row(project: ProjectId, host: &str, port: u16, public_key: &str) -> AProjectSshHostKey {
    AProjectSshHostKey {
        id: Set(ProjectSshHostKeyId::now_v7()),
        project: Set(project),
        host: Set(host.to_lowercase()),
        port: Set(i32::from(port)),
        public_key: Set(public_key.to_owned()),
        learned_at: Set(gradient_types::now()),
        changed_at: Set(None),
    }
}

fn host_columns() -> [CProjectSshHostKey; 3] {
    [
        CProjectSshHostKey::Project,
        CProjectSshHostKey::Host,
        CProjectSshHostKey::Port,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use gradient_fixtures::mock::mock_db;
    use gradient_wire::types::{GitServerSshHostKey, GitSshHostKeyObservation};
    use sea_orm::MockExecResult;

    const SAVED_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAISAVED";
    const NEW_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINEW";

    fn learned(host: &str, public_key: &str) -> GitSshHostKeyObservation {
        GitSshHostKeyObservation::UnknownHostKeyAcceptedForSaving {
            key: GitServerSshHostKey {
                host: host.into(),
                port: 22,
                public_key: public_key.into(),
            },
        }
    }

    fn saved_row(project: ProjectId) -> MProjectSshHostKey {
        MProjectSshHostKey {
            id: ProjectSshHostKeyId::now_v7(),
            project,
            host: "git.elektrat.de".into(),
            port: 22,
            public_key: SAVED_KEY.into(),
            ..Default::default()
        }
    }

    fn all_sql(db: sea_orm::DatabaseConnection) -> Vec<(String, String)> {
        db.into_transaction_log()
            .iter()
            .flat_map(|t| t.statements())
            .map(|s| {
                (
                    s.sql.clone(),
                    format!("{:?}", s.values.as_ref().map(|v| &v.0)),
                )
            })
            .collect()
    }

    #[tokio::test]
    async fn a_learned_key_is_inserted_without_overwriting_a_saved_one() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        apply_project_ssh_host_key_observations(
            &db,
            project,
            &[learned("git.elektrat.de", NEW_KEY)],
        )
        .await
        .expect("apply");

        let sql = all_sql(db);
        assert_eq!(sql.len(), 1, "{sql:?}");
        assert!(
            sql[0]
                .0
                .contains(r#"ON CONFLICT ("project", "host", "port") DO NOTHING"#),
            "{sql:?}"
        );
    }

    #[tokio::test]
    async fn a_learned_key_conflicting_with_a_saved_one_marks_the_host_changed() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_exec_results([
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 0,
                },
                MockExecResult {
                    last_insert_id: 0,
                    rows_affected: 1,
                },
            ])
            .append_query_results([vec![saved_row(project)]])
            .into_connection();
        apply_project_ssh_host_key_observations(
            &db,
            project,
            &[learned("git.elektrat.de", NEW_KEY)],
        )
        .await
        .expect("apply");

        let sql = all_sql(db);
        let update = sql.last().expect("statements");
        assert!(update.0.starts_with("UPDATE"), "{sql:?}");
        assert!(update.0.contains(r#""changed_at""#), "{sql:?}");
    }

    #[tokio::test]
    async fn a_learned_key_equal_to_the_saved_one_changes_nothing() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            }])
            .append_query_results([vec![saved_row(project)]])
            .into_connection();
        apply_project_ssh_host_key_observations(
            &db,
            project,
            &[learned("git.elektrat.de", SAVED_KEY)],
        )
        .await
        .expect("apply");

        assert!(
            all_sql(db)
                .iter()
                .all(|(sql, _)| !sql.starts_with("UPDATE")),
            "an equal key must not flag the host"
        );
    }

    #[tokio::test]
    async fn hosts_are_saved_lowercase() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        apply_project_ssh_host_key_observations(
            &db,
            project,
            &[learned("Git.Elektrat.DE", NEW_KEY)],
        )
        .await
        .expect("apply");

        let values = &all_sql(db)[0].1;
        assert!(values.contains("git.elektrat.de"), "{values}");
        assert!(!values.contains("Git.Elektrat.DE"), "{values}");
    }

    #[tokio::test]
    async fn a_changed_host_is_flagged() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_exec_results([MockExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            }])
            .into_connection();
        apply_project_ssh_host_key_observations(
            &db,
            project,
            &[GitSshHostKeyObservation::HostKeyMismatchConnectionRefused {
                host: "GIT.elektrat.de".into(),
                port: 2222,
            }],
        )
        .await
        .expect("apply");

        let sql = all_sql(db);
        assert!(sql[0].0.starts_with("UPDATE"), "{sql:?}");
        assert!(sql[0].0.contains(r#""changed_at""#), "{sql:?}");
        assert!(sql[0].1.contains("git.elektrat.de"), "{sql:?}");
    }

    #[tokio::test]
    async fn saving_a_key_clears_the_changed_flag() {
        let project = ProjectId::now_v7();
        let db = mock_db()
            .append_query_results([vec![saved_row(project)]])
            .into_connection();
        upsert(&db, project, "Git.Elektrat.de", 22, NEW_KEY)
            .await
            .expect("upsert");

        let sql = all_sql(db);
        assert!(
            sql[0]
                .0
                .contains(r#"ON CONFLICT ("project", "host", "port") DO UPDATE"#),
            "{sql:?}"
        );
        assert!(
            sql[0]
                .0
                .contains(r#""changed_at" = "excluded"."changed_at""#),
            "{sql:?}"
        );
        assert!(sql[0].1.contains("git.elektrat.de"), "{sql:?}");
    }
}
