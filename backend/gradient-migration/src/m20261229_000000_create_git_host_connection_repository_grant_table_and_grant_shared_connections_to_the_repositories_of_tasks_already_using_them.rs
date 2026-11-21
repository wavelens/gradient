/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

// Tasks that already use a connection owned by the instance or by a team of their project keep that connection. These tasks receive a grant for their own repository. Tasks with no owner/repo path in the repository URL lose the shared connection and stop reporting status.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

#[derive(DeriveMigrationName)]
pub struct Migration;

const REPOSITORY_GRANT_SOURCE_CREATED_BY_UPGRADE: i16 = 1;

const CREATE_GIT_HOST_CONNECTION_REPOSITORY_GRANT_TABLE_WITH_UNIQUE_CONSTRAINT_AND_PROJECT_INDEX_SQL: &str = r#"
CREATE TABLE public.git_host_connection_repository_grant (
    id uuid PRIMARY KEY,
    git_host_connection uuid NOT NULL
        REFERENCES public.git_host_connection(id) ON UPDATE CASCADE ON DELETE CASCADE,
    project uuid NOT NULL REFERENCES public.project(id) ON UPDATE CASCADE ON DELETE CASCADE,
    repository_owner text NOT NULL,
    repository_name text,
    source smallint NOT NULL,
    created_by uuid,
    created_at timestamp without time zone NOT NULL,
    CONSTRAINT "fk-git_host_connection_repository_grant-created_by"
        FOREIGN KEY (created_by) REFERENCES public."user"(id) ON DELETE SET NULL,
    CONSTRAINT git_host_connection_repository_grant_unique
        UNIQUE NULLS NOT DISTINCT (git_host_connection, project, repository_owner, repository_name)
);
CREATE INDEX git_host_connection_repository_grant_project
    ON public.git_host_connection_repository_grant (project);
"#;

const LIST_TASKS_AND_OPEN_PULL_REQUEST_ACTIONS_USING_GIT_HOST_CONNECTION: &str = r#"
SELECT t.id AS task, t.project, t.repository, c.id AS connection, true AS attached
FROM public.task t
JOIN public.git_host_connection c ON c.id = t.git_host_connection
JOIN public.project p ON p.id = t.project
WHERE c.project IS NULL AND (c.owner_team IS NULL OR c.owner_team = p.owner_team)
UNION ALL
SELECT t.id, t.project, t.repository, c.id, false
FROM public.task_action a
JOIN public.task t ON t.id = a.task
JOIN public.git_host_connection c ON c.id::text = a.config->>'git_host_connection_id'
JOIN public.project p ON p.id = t.project
WHERE a.action_type = 3 AND a.active AND c.project IS NULL
    AND (c.owner_team IS NULL OR c.owner_team = p.owner_team)
"#;

// A copy of the repository URL parser from the time this migration was written. Keep this copy fixed on purpose. The behavior of this migration does not change with a later change to the parser in the code.
fn parse_owner_and_repository_from_git_repository_url(
    repository_url: &str,
) -> Option<(String, String)> {
    let clone_url = repository_url
        .split_once('?')
        .map_or(repository_url, |(url, _)| url);
    let url = clone_url.strip_prefix("git+").unwrap_or(clone_url);
    let path = match ["https://", "http://", "git://", "ssh://"]
        .iter()
        .find_map(|scheme| url.strip_prefix(scheme))
    {
        Some(rest) => rest.split_once('/')?.1,
        None => &url[url.find(':')? + 1..],
    };
    let path = path.trim_end_matches('/').trim_end_matches(".git");
    if !path.split('/').all(is_nonempty_url_path_segment_that_is_not_dot_or_dot_dot_and_has_only_letters_digits_dot_underscore_or_hyphen) {
        return None;
    }
    let (owner, repository) = path.split_once('/')?;
    Some((owner.to_owned(), repository.to_owned()))
}

fn is_nonempty_url_path_segment_that_is_not_dot_or_dot_dot_and_has_only_letters_digits_dot_underscore_or_hyphen(
    segment: &str,
) -> bool {
    !matches!(segment, "" | "." | "..")
        && segment
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        let db = manager.get_connection();
        let backend = db.get_database_backend();
        db.execute_unprepared("SET LOCAL lock_timeout = '5s';")
            .await?;
        db.execute_unprepared(CREATE_GIT_HOST_CONNECTION_REPOSITORY_GRANT_TABLE_WITH_UNIQUE_CONSTRAINT_AND_PROJECT_INDEX_SQL).await?;
        let uses = db
            .query_all_raw(Statement::from_string(
                backend,
                LIST_TASKS_AND_OPEN_PULL_REQUEST_ACTIONS_USING_GIT_HOST_CONNECTION,
            ))
            .await?;
        for row in uses {
            let task: uuid::Uuid = row.try_get("", "task")?;
            let project: uuid::Uuid = row.try_get("", "project")?;
            let repository: String = row.try_get("", "repository")?;
            let connection: uuid::Uuid = row.try_get("", "connection")?;
            let attached: bool = row.try_get("", "attached")?;
            match parse_owner_and_repository_from_git_repository_url(&repository) {
                Some((owner, name)) => {
                    db.execute_raw(Statement::from_sql_and_values(
                        backend,
                        "INSERT INTO public.git_host_connection_repository_grant \
                         (id, git_host_connection, project, repository_owner, repository_name, \
                         source, created_by, created_at) \
                         VALUES ($1, $2, $3, $4, $5, $6, NULL, now()) ON CONFLICT DO NOTHING",
                        [
                            uuid::Uuid::now_v7().into(),
                            connection.into(),
                            project.into(),
                            owner.to_lowercase().into(),
                            name.to_lowercase().into(),
                            REPOSITORY_GRANT_SOURCE_CREATED_BY_UPGRADE.into(),
                        ],
                    ))
                    .await?;
                }
                None if attached => {
                    db.execute_raw(Statement::from_sql_and_values(
                        backend,
                        "UPDATE public.task SET git_host_connection = NULL, report_status = false \
                         WHERE id = $1",
                        [task.into()],
                    ))
                    .await?;
                }
                None => {}
            }
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .get_connection()
            .execute_unprepared(
                "SET LOCAL lock_timeout = '5s'; \
                 DROP TABLE public.git_host_connection_repository_grant;",
            )
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::parse_owner_and_repository_from_git_repository_url;

    #[test]
    fn test_frozen_url_parser_reads_owner_and_repository_from_https_ssh_and_scp_urls() {
        for url in [
            "https://github.com/acme/infra",
            "https://github.com/acme/infra.git",
            "https://github.com/acme/infra/",
            "https://ci:secret@github.com:8443/acme/infra",
            "http://github.com/acme/infra",
            "git://github.com/acme/infra.git",
            "git+https://github.com/acme/infra.git",
            "ssh://git@github.com:2222/acme/infra.git",
            "git+ssh://git@github.com/acme/infra",
            "git@github.com:acme/infra.git",
            "https://github.com/acme/infra?dir=nix",
        ] {
            assert_eq!(
                parse_owner_and_repository_from_git_repository_url(url),
                Some(("acme".into(), "infra".into())),
                "{url}"
            );
        }
        assert_eq!(
            parse_owner_and_repository_from_git_repository_url("https://GitHub.com/Acme/Infra.git"),
            Some(("Acme".into(), "Infra".into()))
        );
        assert_eq!(
            parse_owner_and_repository_from_git_repository_url(
                "git@gitlab.example.com:group/sub/repo.git"
            ),
            Some(("group".into(), "sub/repo".into()))
        );
    }

    #[test]
    fn frozen_url_parser_returns_none_for_urls_without_owner_and_repository_path_or_with_invalid_segments()
     {
        for url in [
            "https://github.com",
            "https://github.com/acme",
            "no-colon-and-no-scheme",
            "https://github.com/a/../b",
            "https://github.com/a//b",
            "https://github.com/a/b%2F..",
        ] {
            assert_eq!(
                parse_owner_and_repository_from_git_repository_url(url),
                None,
                "{url}"
            );
        }
    }
}
